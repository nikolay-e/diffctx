// Seed repositories for the Tier-1 languages: one changed definition (`total`),
// two known callers and a test, built from a set of knobs so a transformation
// is a difference between two knob sets. The expected callers come from
// reading the generated files, never from running diffctx.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

pub type Files = BTreeMap<String, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    TypeScript,
    Rust,
    Kotlin,
}

pub const TIER1: [Lang; 4] = [Lang::Python, Lang::TypeScript, Lang::Rust, Lang::Kotlin];

impl Lang {
    pub fn label(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Kotlin => "kotlin",
        }
    }
}

// What the range's commit does to `total`: a new parameter, or a new body
// behind the same signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Signature,
    Body,
}

// Everything a transformation may vary. The default is the plain seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Knobs {
    pub indent: usize,
    pub blank_lines: usize,
    pub trailing_comment: bool,
    pub spaces: usize,
    pub paren: bool,
    pub interp: bool,
    pub extra_in_fn: usize,
    pub extra_defs: usize,
    pub extra_files: usize,
    pub decoy_package: bool,
    pub decoy_root: bool,
    pub decoy_text: bool,
    pub rename_prior: bool,
    pub rename_in_range: bool,
    pub move_helper: bool,
    pub history_split: usize,
    pub reverse_write_order: bool,
    pub extra_callers: usize,
    pub stale: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            indent: 4,
            blank_lines: 0,
            trailing_comment: false,
            spaces: 0,
            paren: false,
            interp: false,
            extra_in_fn: 0,
            extra_defs: 0,
            extra_files: 0,
            decoy_package: false,
            decoy_root: false,
            decoy_text: false,
            rename_prior: false,
            rename_in_range: false,
            move_helper: false,
            history_split: 1,
            reverse_write_order: false,
            extra_callers: 0,
            stale: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Step {
    pub renames: Vec<(String, String)>,
    pub removes: Vec<String>,
    pub writes: Files,
}

#[derive(Clone, Debug)]
pub struct Seed {
    pub lang: Lang,
    pub steps: Vec<Step>,
    pub reverse_write_order: bool,
    pub target: &'static str,
    pub target_path: String,
    // (path, symbol) of every caller of `target`, as the files at the head
    // spell them.
    pub callers: Vec<(String, String)>,
    // (caller path, test path): the test imports the caller and calls it.
    pub tested: Vec<(String, String)>,
    // Paths that never relate to `target`, whatever the kind.
    pub decoys: Vec<String>,
    // Old to new names of files renamed before the range.
    pub renames: Vec<(String, String)>,
    // Paths the range renames or moves without changing a definition.
    pub moved: Vec<String>,
    pub stale: Option<String>,
}

pub const RANGE: &str = "HEAD~1..HEAD";

impl Seed {
    // The seed as a repository under `dir`, one commit per step.
    pub fn commit_into(&self, dir: &Path) {
        run_git(dir, &["init", "-q", "-b", "main", "."]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "diffctx tests"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        for (index, step) in self.steps.iter().enumerate() {
            for (from, to) in &step.renames {
                if let Some(parent) = dir.join(to).parent() {
                    std::fs::create_dir_all(parent).expect("mkdir");
                }
                run_git(dir, &["mv", from, to]);
            }
            for rel in &step.removes {
                std::fs::remove_file(dir.join(rel)).expect("remove");
            }
            let mut writes: Vec<(&String, &String)> = step.writes.iter().collect();
            if self.reverse_write_order {
                writes.reverse();
            }
            for (rel, content) in writes {
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

    // The path a name has at the head after the renames before the range.
    pub fn renamed(&self, path: &str) -> String {
        self.renames
            .iter()
            .find(|(old, _)| old == path)
            .map_or_else(|| path.to_string(), |(_, new)| new.clone())
    }
}

// A scratch repository must not depend on the machine's git configuration.
pub fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn seed(lang: Lang, edit: Edit, knobs: &Knobs) -> Seed {
    Build {
        lang,
        edit,
        k: knobs.clone(),
    }
    .seed()
}

struct Build {
    lang: Lang,
    edit: Edit,
    k: Knobs,
}

impl Build {
    fn seed(&self) -> Seed {
        let base = self.initial_files();
        let mut steps = self.split(&base);
        let mut renames = Vec::new();
        if self.k.rename_prior {
            renames.push((self.path("report"), self.path("summary")));
            steps.push(Step {
                renames: renames.clone(),
                writes: self.rust_lib(true, false),
                ..Step::default()
            });
        }
        let mut change = Step {
            writes: self.change_files(),
            ..Step::default()
        };
        let mut moved = Vec::new();
        if self.k.rename_in_range {
            let from = self.util_path(false);
            let to = self.util_path(true);
            moved.push(from.clone());
            moved.push(to.clone());
            change.renames.push((from, to));
        }
        if self.k.move_helper {
            moved.push(self.path("rates"));
        }
        if self.k.stale {
            change.removes.push("scripts/rotate.sh".to_string());
        }
        steps.push(change);
        self.describe(steps, renames, moved)
    }

    fn describe(
        &self,
        steps: Vec<Step>,
        renames: Vec<(String, String)>,
        moved: Vec<String>,
    ) -> Seed {
        let report = if self.k.rename_prior {
            self.path("summary")
        } else {
            self.path("report")
        };
        let mut callers = vec![
            (self.path("checkout"), "charge".to_string()),
            (report, "summarize".to_string()),
        ];
        for i in 0..self.k.extra_callers {
            callers.push((self.path(&format!("caller_{i}")), self.caller_name(i)));
        }
        Seed {
            lang: self.lang,
            steps,
            reverse_write_order: self.k.reverse_write_order,
            target: "total",
            target_path: self.path("pricing"),
            callers,
            tested: vec![(self.path("checkout"), self.test_path())],
            decoys: self.decoy_paths(),
            renames,
            moved,
            stale: self.k.stale.then(|| "ops.md".to_string()),
        }
    }

    // The initial files spread over `history_split` commits.
    fn split(&self, base: &Files) -> Vec<Step> {
        let parts = self.k.history_split.max(1);
        let mut chunks = vec![Files::new(); parts];
        for (index, (rel, content)) in base.iter().enumerate() {
            chunks[index % parts].insert(rel.clone(), content.clone());
        }
        chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|writes| Step {
                writes,
                ..Step::default()
            })
            .collect()
    }

    fn initial_files(&self) -> Files {
        let mut files = Files::new();
        let mut put = |rel: String, content: String| {
            files.insert(rel, content);
        };
        for (rel, content) in self.manifests() {
            put(rel, content);
        }
        put(self.path("pricing"), self.pricing(None));
        put(self.path("checkout"), self.caller_file("charge", "cart"));
        put(self.path("report"), self.caller_file("summarize", "orders"));
        put(self.util_path(false), self.util());
        put(self.test_path(), self.test_file());
        for i in 0..self.k.extra_callers {
            put(
                self.path(&format!("caller_{i}")),
                self.caller_file(&self.caller_name(i), "cart"),
            );
        }
        for i in 0..self.k.extra_files {
            put(self.path(&format!("unrelated_{i}")), self.unrelated_file(i));
        }
        for (rel, content) in self.rust_lib(false, false) {
            put(rel, content);
        }
        for (rel, content) in self.decoys() {
            put(rel, content);
        }
        if self.k.stale {
            put("scripts/rotate.sh".to_string(), "echo rotate\n".to_string());
            put(
                "ops.md".to_string(),
                "Run scripts/rotate.sh weekly.\n".to_string(),
            );
        }
        files
    }

    fn change_files(&self) -> Files {
        let mut files = Files::new();
        files.insert(self.path("pricing"), self.pricing(Some(self.edit)));
        if self.k.move_helper {
            files.insert(self.path("rates"), self.rates_file());
            for (rel, content) in self.rust_lib(self.k.rename_prior, true) {
                files.insert(rel, content);
            }
        }
        files
    }

    // ---- paths ----

    fn path(&self, stem: &str) -> String {
        match self.lang {
            Lang::Python => format!("shop/{stem}.py"),
            Lang::TypeScript => format!("src/{stem}.ts"),
            Lang::Rust => format!("src/{stem}.rs"),
            Lang::Kotlin => format!("src/main/kotlin/shop/{}.kt", pascal(stem)),
        }
    }

    fn util_path(&self, renamed: bool) -> String {
        match (self.lang, renamed) {
            (Lang::Rust, false) => "src/bin/tool.rs".to_string(),
            (Lang::Rust, true) => "src/bin/tool2.rs".to_string(),
            (_, false) => self.path("util"),
            (_, true) => self.path("helpers"),
        }
    }

    fn test_path(&self) -> String {
        match self.lang {
            Lang::Python => "tests/test_checkout.py".to_string(),
            Lang::TypeScript => "tests/checkout.test.ts".to_string(),
            Lang::Rust => "tests/checkout.rs".to_string(),
            Lang::Kotlin => "src/test/kotlin/shop/CheckoutTest.kt".to_string(),
        }
    }

    fn caller_name(&self, i: usize) -> String {
        match self.lang {
            Lang::Python | Lang::Rust => format!("use_{i}"),
            Lang::TypeScript | Lang::Kotlin => format!("use{i}"),
        }
    }

    // ---- layout ----

    fn ind(&self) -> String {
        " ".repeat(self.k.indent)
    }

    fn sep(&self) -> String {
        let base = if self.lang == Lang::Python { 2 } else { 1 };
        "\n".repeat(base + self.k.blank_lines)
    }

    fn join(&self, parts: &[String]) -> String {
        parts.join(&self.sep())
    }

    fn comment(&self) -> &'static str {
        if self.lang == Lang::Python { "#" } else { "//" }
    }

    // ---- code ----

    fn header(&self) -> String {
        match self.lang {
            Lang::Python => "from shop.pricing import total\n".to_string(),
            Lang::TypeScript => "import { total } from './pricing';\n".to_string(),
            Lang::Rust => "use crate::pricing::total;\n".to_string(),
            Lang::Kotlin => "package shop\n".to_string(),
        }
    }

    fn package_header(&self) -> Option<String> {
        (self.lang == Lang::Kotlin).then(|| "package shop\n".to_string())
    }

    fn pricing(&self, edit: Option<Edit>) -> String {
        let mut defs = Vec::new();
        defs.extend(self.package_header());
        defs.extend(self.extra_defs(0, self.k.extra_defs / 2));
        defs.push(self.total_def(edit));
        let moved = edit.is_some() && self.k.move_helper;
        if !moved {
            defs.push(self.unused_rate());
        }
        defs.extend(self.extra_defs(self.k.extra_defs / 2, self.k.extra_defs));
        self.join(&defs)
    }

    fn total_def(&self, edit: Option<Edit>) -> String {
        let i = self.ind();
        match (self.lang, edit) {
            (Lang::Python, None) => format!("def total(items):\n{i}return sum(items)\n"),
            (Lang::Python, Some(Edit::Signature)) => {
                format!("def total(items, tax=0):\n{i}return sum(items) + tax\n")
            }
            (Lang::Python, Some(Edit::Body)) => {
                format!("def total(items):\n{i}return sum(items) * 2\n")
            }
            (Lang::TypeScript, None) => format!(
                "export function total(items: number[]) {{\n{i}return items.reduce((a, b) => a + b, 0);\n}}\n"
            ),
            (Lang::TypeScript, Some(Edit::Signature)) => format!(
                "export function total(items: number[], tax = 0) {{\n{i}return items.reduce((a, b) => a + b, 0) + tax;\n}}\n"
            ),
            (Lang::TypeScript, Some(Edit::Body)) => format!(
                "export function total(items: number[]) {{\n{i}return items.reduce((a, b) => a + b, 0) * 2;\n}}\n"
            ),
            (Lang::Rust, None) => {
                format!("pub fn total(items: &[i64]) -> i64 {{\n{i}items.iter().sum()\n}}\n")
            }
            (Lang::Rust, Some(Edit::Signature)) => format!(
                "pub fn total(items: &[i64], tax: i64) -> i64 {{\n{i}items.iter().sum::<i64>() + tax\n}}\n"
            ),
            (Lang::Rust, Some(Edit::Body)) => format!(
                "pub fn total(items: &[i64]) -> i64 {{\n{i}items.iter().sum::<i64>() * 2\n}}\n"
            ),
            (Lang::Kotlin, None) => {
                format!("fun total(items: List<Int>): Int {{\n{i}return items.sum()\n}}\n")
            }
            (Lang::Kotlin, Some(Edit::Signature)) => format!(
                "fun total(items: List<Int>, tax: Int = 0): Int {{\n{i}return items.sum() + tax\n}}\n"
            ),
            (Lang::Kotlin, Some(Edit::Body)) => {
                format!("fun total(items: List<Int>): Int {{\n{i}return items.sum() * 2\n}}\n")
            }
        }
    }

    fn unused_rate(&self) -> String {
        let i = self.ind();
        match self.lang {
            Lang::Python => format!("def unused_rate():\n{i}return 1\n"),
            Lang::TypeScript => format!("export function unusedRate() {{\n{i}return 1;\n}}\n"),
            Lang::Rust => format!("pub fn unused_rate() -> i64 {{\n{i}1\n}}\n"),
            Lang::Kotlin => format!("fun unusedRate(): Int {{\n{i}return 1\n}}\n"),
        }
    }

    fn rates_file(&self) -> String {
        let mut parts = Vec::new();
        parts.extend(self.package_header());
        parts.push(self.unused_rate());
        self.join(&parts)
    }

    fn util(&self) -> String {
        let i = self.ind();
        match self.lang {
            Lang::Python => format!("def helper(x):\n{i}return x\n"),
            Lang::TypeScript => format!("export function helper(x: number) {{\n{i}return x;\n}}\n"),
            Lang::Rust => format!("fn main() {{\n{i}println!(\"tool\");\n}}\n"),
            Lang::Kotlin => self.join(&[
                "package shop\n".to_string(),
                format!("fun helper(x: Int): Int {{\n{i}return x\n}}\n"),
            ]),
        }
    }

    fn unrelated_stmt(&self, n: usize) -> String {
        match self.lang {
            Lang::Python => format!("unrelated_{n} = {n}"),
            Lang::TypeScript => format!("const unrelated_{n} = {n};"),
            Lang::Rust => format!("let unrelated_{n} = {n};"),
            Lang::Kotlin => format!("val unrelated_{n} = {n}"),
        }
    }

    // Top-level definitions nothing refers to, alternating functions and
    // constants, numbered `from..to`.
    fn extra_defs(&self, from: usize, to: usize) -> Vec<String> {
        let i = self.ind();
        (from..to)
            .map(|n| match (self.lang, n % 2 == 0) {
                (Lang::Python, true) => format!("def unrelated_fn_{n}(x):\n{i}return x + {n}\n"),
                (Lang::Python, false) => format!("UNRELATED_{n} = {n}\n"),
                (Lang::TypeScript, true) => format!(
                    "export function unrelatedFn{n}(x: number) {{\n{i}return x + {n};\n}}\n"
                ),
                (Lang::TypeScript, false) => format!("export const UNRELATED_{n} = {n};\n"),
                (Lang::Rust, true) => {
                    format!("pub fn unrelated_fn_{n}(x: i64) -> i64 {{\n{i}x + {n}\n}}\n")
                }
                (Lang::Rust, false) => format!("pub const UNRELATED_{n}: i64 = {n};\n"),
                (Lang::Kotlin, true) => {
                    format!("fun unrelatedFn{n}(x: Int): Int {{\n{i}return x + {n}\n}}\n")
                }
                (Lang::Kotlin, false) => format!("const val UNRELATED_{n} = {n}\n"),
            })
            .collect()
    }

    fn unrelated_file(&self, n: usize) -> String {
        let i = self.ind();
        match self.lang {
            Lang::Python => format!("def unrelated_{n}(x):\n{i}return x\n"),
            Lang::TypeScript => {
                format!("export function unrelated{n}(x: number) {{\n{i}return x;\n}}\n")
            }
            Lang::Rust => format!("pub fn unrelated_{n}(x: i64) -> i64 {{\n{i}x\n}}\n"),
            Lang::Kotlin => self.join(&[
                "package shop\n".to_string(),
                format!("fun unrelated{n}(x: Int): Int {{\n{i}return x\n}}\n"),
            ]),
        }
    }

    // The call of `total` as the knobs spell it.
    fn call_expr(&self, arg: &str) -> String {
        let pad = " ".repeat(self.k.spaces);
        let mut expr = format!("total({pad}{arg}{pad})");
        if self.k.interp {
            expr = match self.lang {
                Lang::Python => format!("f\"{{{expr}}}\""),
                Lang::TypeScript => format!("`${{{expr}}}`"),
                Lang::Kotlin => format!("\"${{{expr}}}\""),
                Lang::Rust => expr,
            };
        }
        if self.k.paren {
            expr = format!("({expr})");
        }
        expr
    }

    // A function whose one meaningful statement calls `total`, with the
    // unrelated statements the knobs ask for on both sides of it.
    fn caller_def(&self, name: &str, arg: &str) -> String {
        let i = self.ind();
        let note = if self.k.trailing_comment {
            format!(" {} note", self.comment())
        } else {
            String::new()
        };
        let expr = self.call_expr(arg);
        let before = self.k.extra_in_fn / 2;
        let after = self.k.extra_in_fn - before;
        let unrelated = |from: usize, to: usize| -> String {
            (from..to)
                .map(|n| format!("{i}{}\n", self.unrelated_stmt(n)))
                .collect::<Vec<_>>()
                .concat()
        };
        let (open, call, ret, close) = match self.lang {
            Lang::Python => (
                format!("def {name}({arg}):\n"),
                format!("{i}result = {expr}{note}\n"),
                format!("{i}return result\n"),
                String::new(),
            ),
            Lang::TypeScript => (
                format!("export function {name}({arg}: number[]) {{\n"),
                format!("{i}const result = {expr};{note}\n"),
                format!("{i}return result;\n"),
                "}\n".to_string(),
            ),
            Lang::Rust => (
                format!("pub fn {name}({arg}: &[i64]) -> i64 {{\n"),
                format!("{i}let result = {expr};{note}\n"),
                format!("{i}result\n"),
                "}\n".to_string(),
            ),
            Lang::Kotlin => (
                format!("fun {name}({arg}: List<Int>): Any {{\n"),
                format!("{i}val result = {expr}{note}\n"),
                format!("{i}return result\n"),
                "}\n".to_string(),
            ),
        };
        format!(
            "{open}{}{call}{}{ret}{close}",
            unrelated(0, before),
            unrelated(before, before + after)
        )
    }

    fn caller_file(&self, name: &str, arg: &str) -> String {
        let mut parts = vec![self.header()];
        parts.extend(self.extra_defs(0, self.k.extra_defs / 2));
        parts.push(self.caller_def(name, arg));
        parts.extend(self.extra_defs(self.k.extra_defs / 2, self.k.extra_defs));
        self.join(&parts)
    }

    fn test_file(&self) -> String {
        let i = self.ind();
        let parts = match self.lang {
            Lang::Python => [
                "from shop.checkout import charge\n".to_string(),
                format!("def test_charge():\n{i}assert charge([1]) == 1\n"),
            ],
            Lang::TypeScript => [
                "import { charge } from '../src/checkout';\n".to_string(),
                format!("test('charges', () => {{\n{i}expect(charge([1])).toBe(1);\n}});\n"),
            ],
            Lang::Rust => [
                "use shop::checkout::charge;\n".to_string(),
                format!("#[test]\nfn charges() {{\n{i}assert_eq!(charge(&[1]), 1);\n}}\n"),
            ],
            Lang::Kotlin => [
                "package shop\n\nimport kotlin.test.Test\n".to_string(),
                format!(
                    "class CheckoutTest {{\n{i}@Test\n{i}fun charges() {{\n{i}{i}charge(listOf(1))\n{i}}}\n}}\n"
                ),
            ],
        };
        self.join(&parts)
    }

    // ---- manifests and module lists ----

    fn manifests(&self) -> Vec<(String, String)> {
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        match self.lang {
            Lang::Python => vec![pair(
                "pyproject.toml",
                "[project]\nname = \"shop\"\nversion = \"0.1.0\"\n",
            )],
            Lang::TypeScript => vec![
                pair(
                    "package.json",
                    "{\n  \"name\": \"shop\",\n  \"version\": \"1.0.0\"\n}\n",
                ),
                pair("tsconfig.json", "{\n  \"compilerOptions\": {}\n}\n"),
            ],
            Lang::Rust => vec![pair(
                "Cargo.toml",
                "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )],
            Lang::Kotlin => vec![
                pair("settings.gradle.kts", "rootProject.name = \"shop\"\n"),
                pair(
                    "build.gradle.kts",
                    "plugins {\n    kotlin(\"jvm\") version \"2.0.0\"\n}\n",
                ),
            ],
        }
    }

    // `src/lib.rs`, which declares every module of the Rust seed; nothing for
    // the other languages.
    fn rust_lib(&self, renamed: bool, with_rates: bool) -> Files {
        let mut files = Files::new();
        if self.lang != Lang::Rust {
            return files;
        }
        let mut modules = vec![
            "pricing".to_string(),
            "checkout".to_string(),
            if renamed { "summary" } else { "report" }.to_string(),
        ];
        if with_rates {
            modules.push("rates".to_string());
        }
        for i in 0..self.k.extra_callers {
            modules.push(format!("caller_{i}"));
        }
        for i in 0..self.k.extra_files {
            modules.push(format!("unrelated_{i}"));
        }
        if self.k.decoy_package {
            modules.push("legacy".to_string());
            modules.push("invoice".to_string());
        }
        let mut text = String::new();
        for module in &modules {
            let _ = writeln!(text, "pub mod {module};");
        }
        files.insert("src/lib.rs".to_string(), text);
        files
    }

    // ---- decoys ----

    fn decoys(&self) -> Files {
        let mut files = Files::new();
        if self.k.decoy_package {
            files.extend(self.decoy_package());
        }
        if self.k.decoy_root {
            files.extend(self.decoy_root());
        }
        if self.k.decoy_text {
            files.extend(self.decoy_text());
        }
        files
    }

    fn decoy_paths(&self) -> Vec<String> {
        self.decoys().into_keys().collect()
    }

    fn decoy_package(&self) -> Files {
        let i = self.ind();
        let entries: Vec<(&str, String)> = match self.lang {
            Lang::Python => vec![
                (
                    "legacy/tally.py",
                    format!("def total(rows):\n{i}return len(rows)\n"),
                ),
                (
                    "legacy/invoice.py",
                    format!(
                        "from legacy.tally import total\n{}def bill(rows):\n{i}return total(rows)\n",
                        self.sep()
                    ),
                ),
            ],
            Lang::TypeScript => vec![
                (
                    "legacy/package.json",
                    "{\n  \"name\": \"legacy\"\n}\n".to_string(),
                ),
                (
                    "legacy/src/pricing.ts",
                    format!(
                        "export function total(rows: number[]) {{\n{i}return rows.length;\n}}\n"
                    ),
                ),
                (
                    "legacy/src/invoice.ts",
                    format!(
                        "import {{ total }} from './pricing';\n{}export function bill(rows: number[]) {{\n{i}return total(rows);\n}}\n",
                        self.sep()
                    ),
                ),
            ],
            Lang::Rust => vec![
                (
                    "src/legacy.rs",
                    format!("pub fn total(rows: &[i64]) -> i64 {{\n{i}rows.len() as i64\n}}\n"),
                ),
                (
                    "src/invoice.rs",
                    format!(
                        "use crate::legacy::total;\n{}pub fn bill(rows: &[i64]) -> i64 {{\n{i}total(rows)\n}}\n",
                        self.sep()
                    ),
                ),
            ],
            Lang::Kotlin => vec![
                (
                    "src/main/kotlin/legacy/Tally.kt",
                    format!(
                        "package legacy\n{}fun total(rows: List<Int>): Int {{\n{i}return rows.size\n}}\n",
                        self.sep()
                    ),
                ),
                (
                    "src/main/kotlin/legacy/Invoice.kt",
                    format!(
                        "package legacy\n{}fun bill(rows: List<Int>): Int {{\n{i}return total(rows)\n}}\n",
                        self.sep()
                    ),
                ),
            ],
        };
        entries
            .into_iter()
            .map(|(rel, content)| (rel.to_string(), content))
            .collect()
    }

    // A second project with its own manifest that spells the same module
    // names and the same function, and never depends on this one.
    fn decoy_root(&self) -> Files {
        let i = self.ind();
        let entries: Vec<(&str, String)> = match self.lang {
            Lang::Python => vec![
                (
                    "other/pyproject.toml",
                    "[project]\nname = \"other\"\n".to_string(),
                ),
                (
                    "other/shop/pricing.py",
                    format!("def total(items):\n{i}return len(items)\n"),
                ),
                (
                    "other/shop/checkout.py",
                    format!(
                        "from shop.pricing import total\n{}def charge(cart):\n{i}return total(cart)\n",
                        self.sep()
                    ),
                ),
                (
                    "other/shop/use.py",
                    format!("def run(x):\n{i}return x.total()\n"),
                ),
            ],
            Lang::TypeScript => vec![
                (
                    "other/package.json",
                    "{\n  \"name\": \"other\"\n}\n".to_string(),
                ),
                (
                    "other/src/pricing.ts",
                    format!(
                        "export function total(items: number[]) {{\n{i}return items.length;\n}}\n"
                    ),
                ),
                (
                    "other/src/checkout.ts",
                    format!(
                        "import {{ total }} from './pricing';\n{}export function charge(cart: number[]) {{\n{i}return total(cart);\n}}\n",
                        self.sep()
                    ),
                ),
                (
                    "other/src/use.ts",
                    format!("export function run(x: any) {{\n{i}return x.total();\n}}\n"),
                ),
            ],
            Lang::Rust => vec![
                (
                    "other/Cargo.toml",
                    "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                        .to_string(),
                ),
                (
                    "other/src/lib.rs",
                    "pub mod checkout;\npub mod pricing;\n".to_string(),
                ),
                (
                    "other/src/pricing.rs",
                    format!("pub fn total(items: &[i64]) -> i64 {{\n{i}items.len() as i64\n}}\n"),
                ),
                (
                    "other/src/checkout.rs",
                    format!(
                        "use crate::pricing::total;\n{}pub fn charge(cart: &[i64]) -> i64 {{\n{i}total(cart)\n}}\n",
                        self.sep()
                    ),
                ),
            ],
            Lang::Kotlin => vec![
                (
                    "other/build.gradle.kts",
                    "plugins {\n    kotlin(\"jvm\") version \"2.0.0\"\n}\n".to_string(),
                ),
                (
                    "other/src/main/kotlin/shop/Pricing.kt",
                    format!(
                        "package shop\n{}fun total(items: List<Int>): Int {{\n{i}return items.size\n}}\n",
                        self.sep()
                    ),
                ),
                (
                    "other/src/main/kotlin/shop/Checkout.kt",
                    format!(
                        "package shop\n{}fun charge(cart: List<Int>): Int {{\n{i}return total(cart)\n}}\n",
                        self.sep()
                    ),
                ),
            ],
        };
        entries
            .into_iter()
            .map(|(rel, content)| (rel.to_string(), content))
            .collect()
    }

    // The name inside a string and a comment: text, not a call.
    fn decoy_text(&self) -> Files {
        let note = self.comment();
        let (rel, ext_header, string) = match self.lang {
            Lang::Python => (
                "notes/pricing_notes.py",
                String::new(),
                "NOTE = \"call total(cart) to price a cart\"\n",
            ),
            Lang::TypeScript => (
                "notes/pricing-notes.ts",
                String::new(),
                "export const NOTE = \"call total(cart) to price a cart\";\n",
            ),
            Lang::Rust => (
                "notes/pricing_notes.rs",
                String::new(),
                "pub const NOTE: &str = \"call total(cart) to price a cart\";\n",
            ),
            Lang::Kotlin => (
                "notes/PricingNotes.kt",
                "package notes\n".to_string(),
                "const val NOTE = \"call total(cart) to price a cart\"\n",
            ),
        };
        let content = format!("{ext_header}{note} total(cart) is documented elsewhere\n{string}");
        Files::from([(rel.to_string(), content)])
    }
}

// `caller_3` to `Caller3`, `pricing` to `Pricing`.
fn pascal(stem: &str) -> String {
    stem.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect()
}
