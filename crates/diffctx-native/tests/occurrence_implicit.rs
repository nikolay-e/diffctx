//! The implicit kinds of a definition, read off the facts the extractors state for
//! small sources: every kind has a seed, and a definition with no row stays empty.
//!
//! What today's facts cannot say, left to step 3: a Python or TypeScript test (no
//! extractor emits `EntryKind::Test` for them), a TypeScript operator, and an
//! `Outside` kind of Python, TypeScript and Kotlin (no such export exists there).

use std::sync::Arc;

use _diffctx::facts::lang::c::CExtractor;
use _diffctx::facts::lang::go::GoExtractor;
use _diffctx::facts::lang::java::JavaExtractor;
use _diffctx::facts::lang::kotlin::KotlinExtractor;
use _diffctx::facts::lang::php::PhpExtractor;
use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::lang::shell::ShellExtractor;
use _diffctx::facts::lang::sql::SqlExtractor;
use _diffctx::facts::lang::terraform::TerraformExtractor;
use _diffctx::facts::lang::typescript::TypeScript;
use _diffctx::facts::{Extractor, FileFacts, ModulePath, ProjectId};
use _diffctx::occurrence::{Implicit, Known, implicit_kinds};
use _diffctx::snapshot::{RepoPath, RunBudget};

fn kinds_in(
    path: &str,
    extractor: &dyn Extractor,
    source: &str,
    at: &str,
    known: Known,
) -> Vec<Implicit> {
    let file = FileFacts {
        path: RepoPath::new(path).expect("a repository path"),
        project: ProjectId::default(),
        module: ModulePath::new("sample"),
        blob: Arc::new(extractor.extract(source.as_bytes(), &RunBudget::default())),
        outdated_grammar: false,
    };
    let def = file
        .defs()
        .find(|d| {
            let mut parts: Vec<&str> = d.local.container.iter().map(|n| n.as_str()).collect();
            parts.push(d.local.name.as_str());
            parts.join(".") == at
        })
        .unwrap_or_else(|| panic!("no definition {at}"));
    implicit_kinds(&file, def, known)
}

fn kinds_with(extractor: &dyn Extractor, source: &str, at: &str, known: Known) -> Vec<Implicit> {
    kinds_in("src/sample", extractor, source, at, known)
}

fn kinds(extractor: &dyn Extractor, source: &str, at: &str) -> Vec<Implicit> {
    kinds_with(extractor, source, at, Known::default())
}

const PYTEST_FIXTURE: &str = "from pytest import fixture\n\n\n@fixture\ndef client():\n    pass\n";
const SPRING_ROUTE: &str = "package p\n\nimport org.springframework.web.bind.annotation.GetMapping\n\nclass C {\n    @GetMapping(\"/x\")\n    fun list() {\n    }\n}\n";
const NEST_ROUTE: &str = "import { Controller, Get } from '@nestjs/common';\n\n@Controller('items')\nexport class Items {\n  @Get()\n  list() {\n    return [];\n  }\n}\n";
const RUST_ADD: &str = "struct P;\nimpl std::ops::Add for P {\n    type Output = P;\n    fn add(self, o: P) -> P {\n        o\n    }\n}\n";
const RUST_NEXT: &str = "struct P;\nimpl Iterator for P {\n    type Item = u8;\n    fn next(&mut self) -> Option<u8> {\n        None\n    }\n}\n";

type Seed<'a> = (Implicit, &'a dyn Extractor, &'a str, &'a str);

fn seed<'a>(kind: Implicit, e: &'a dyn Extractor, source: &'a str, at: &'a str) -> Seed<'a> {
    (kind, e, source, at)
}

#[test]
fn every_kind_has_a_seed_and_each_seed_reports_its_kind() {
    let seeds = vec![
        seed(
            Implicit::Operator,
            &PythonExtractor,
            "class V:\n    def __add__(self, o):\n        return o\n",
            "V.__add__",
        ),
        seed(Implicit::Operator, &RustExtractor, RUST_ADD, "P.add"),
        seed(
            Implicit::Operator,
            &KotlinExtractor,
            "class V {\n    operator fun plus(o: V): V = o\n}\n",
            "V.plus",
        ),
        seed(
            Implicit::Operator,
            &PhpExtractor,
            "<?php\nclass A {\n    public function __get($n) {}\n}\n",
            "A.__get",
        ),
        seed(
            Implicit::Protocol,
            &PythonExtractor,
            "class V:\n    def __iter__(self):\n        return self\n",
            "V.__iter__",
        ),
        seed(
            Implicit::Protocol,
            &TypeScript,
            "class A {\n  toString() {\n    return 'a';\n  }\n}\n",
            "A.toString",
        ),
        seed(Implicit::Protocol, &RustExtractor, RUST_NEXT, "P.next"),
        seed(
            Implicit::Protocol,
            &KotlinExtractor,
            "class V {\n    override fun toString(): String = \"\"\n}\n",
            "V.toString",
        ),
        seed(
            Implicit::Protocol,
            &JavaExtractor,
            "class A {\n    public String toString() {\n        return \"a\";\n    }\n}\n",
            "A.toString",
        ),
        seed(
            Implicit::Protocol,
            &GoExtractor,
            "package p\n\ntype T struct{}\n\nfunc (t T) String() string { return \"\" }\n",
            "T.String",
        ),
        seed(
            Implicit::Override,
            &PythonExtractor,
            "from typing import override\n\n\nclass B(A):\n    @override\n    def run(self):\n        pass\n",
            "B.run",
        ),
        seed(
            Implicit::Override,
            &TypeScript,
            "class A extends B {\n  override run() {}\n}\n",
            "A.run",
        ),
        seed(
            Implicit::Override,
            &RustExtractor,
            "trait T {\n    fn m(&self);\n}\nstruct S;\nimpl T for S {\n    fn m(&self) {}\n}\n",
            "S.m",
        ),
        seed(
            Implicit::Override,
            &KotlinExtractor,
            "class R : Runnable {\n    override fun run() {}\n}\n",
            "R.run",
        ),
        seed(
            Implicit::Override,
            &GoExtractor,
            "package p\n\ntype T struct{}\n\nfunc (t T) Close() error { return nil }\n",
            "T.Close",
        ),
        seed(Implicit::Entry, &PythonExtractor, PYTEST_FIXTURE, "client"),
        seed(Implicit::Entry, &TypeScript, NEST_ROUTE, "Items.list"),
        seed(Implicit::Entry, &RustExtractor, "fn main() {}\n", "main"),
        seed(Implicit::Entry, &KotlinExtractor, SPRING_ROUTE, "C.list"),
        seed(
            Implicit::Entry,
            &CExtractor,
            "__attribute__((constructor)) static void boot(void) {}\n",
            "boot",
        ),
        seed(
            Implicit::Test,
            &PythonExtractor,
            "def test_works():\n    pass\n",
            "test_works",
        ),
        seed(
            Implicit::Test,
            &KotlinExtractor,
            "class T {\n    @Test\n    fun works() {}\n}\n",
            "T.works",
        ),
        seed(
            Implicit::Outside,
            &RustExtractor,
            "#[no_mangle]\npub extern \"C\" fn exported() {}\n",
            "exported",
        ),
        seed(
            Implicit::Outside,
            &ShellExtractor,
            "#!/bin/bash\necho hi\n",
            "<script>",
        ),
        seed(
            Implicit::Outside,
            &SqlExtractor,
            "CREATE FUNCTION audit() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN NEW;\nEND;\n$$;\n",
            "audit",
        ),
    ];
    for kind in Implicit::ALL {
        let own: Vec<_> = seeds.iter().filter(|seed| seed.0 == kind).collect();
        assert!(!own.is_empty(), "no seed for {}", kind.as_str());
        for (_, extractor, source, at) in own {
            let found = kinds(*extractor, source, at);
            assert!(found.contains(&kind), "{at}: {kind:?} not in {found:?}");
        }
    }
}

#[test]
fn only_the_root_module_variable_and_output_are_set_from_outside() {
    for (source, at) in [
        ("variable \"n\" {\n  type = string\n}\n", "var.n"),
        ("output \"id\" {\n  value = 1\n}\n", "output.id"),
    ] {
        let kinds = |path| kinds_in(path, &TerraformExtractor, source, at, Known::default());
        assert_eq!(kinds("main.tf"), [Implicit::Outside], "{at} of the root");
        assert!(kinds("modules/net/main.tf").is_empty(), "{at} of a module");
    }
}

#[test]
fn a_definition_no_row_names_has_no_kind() {
    assert!(kinds(&PythonExtractor, "def helper():\n    pass\n", "helper").is_empty());
    assert!(kinds(&TypeScript, "function helper() {}\n", "helper").is_empty());
    assert!(kinds(&RustExtractor, "fn helper() {}\n", "helper").is_empty());
    assert!(kinds(&KotlinExtractor, "fun helper() {}\n", "helper").is_empty());
    assert!(kinds(&GoExtractor, "package p\n\nfunc helper() {}\n", "helper").is_empty());
    assert!(
        kinds(
            &TerraformExtractor,
            "module \"net\" {\n  source = \"./net\"\n}\n",
            "module.net"
        )
        .is_empty()
    );
    assert!(
        kinds(
            &SqlExtractor,
            "CREATE FUNCTION plain() RETURNS INTEGER LANGUAGE sql AS $$ SELECT 1 $$;\n",
            "plain"
        )
        .is_empty()
    );
    assert!(kinds(&ShellExtractor, "#!/bin/bash\nbuild() {\n  :\n}\n", "build").is_empty());
}

#[test]
fn a_name_is_a_row_of_its_own_family_only() {
    assert!(kinds(&RustExtractor, "fn __add__() {}\n", "__add__").is_empty());
    assert!(kinds(&RustExtractor, "fn toString() {}\n", "toString").is_empty());
    assert!(kinds(&PythonExtractor, "def toString():\n    pass\n", "toString").is_empty());
}

#[test]
fn a_trait_impl_marks_its_members_and_not_the_type() {
    assert!(kinds(&RustExtractor, RUST_ADD, "P").is_empty());
    assert_eq!(
        kinds(&RustExtractor, RUST_ADD, "P.add"),
        [Implicit::Operator, Implicit::Override]
    );
}

#[test]
fn kinds_come_in_one_order_each_once() {
    let found = kinds(
        &KotlinExtractor,
        "class V {\n    override fun toString(): String = \"\"\n}\n",
        "V.toString",
    );
    assert_eq!(found, [Implicit::Protocol, Implicit::Override]);
    let known = Known {
        dispatched: true,
        consumed_outside: true,
    };
    let found = kinds_with(&RustExtractor, RUST_NEXT, "P.next", known);
    assert_eq!(
        found,
        [Implicit::Protocol, Implicit::Override, Implicit::Outside]
    );
}

#[test]
fn what_decide_knows_is_taken_as_given() {
    let known = Known {
        dispatched: true,
        consumed_outside: false,
    };
    let found = kinds_with(
        &PythonExtractor,
        "def helper():\n    pass\n",
        "helper",
        known,
    );
    assert_eq!(found, [Implicit::Override]);
}

const PY_TEST: &str = "def test_works():\n    pass\n";
const RUST_TEST: &str = "#[test]\nfn works() {}\n";

#[test]
fn a_test_of_a_test_file_is_entered_by_the_runner_the_answer_names() {
    let at = |path: &str| {
        kinds_in(
            path,
            &PythonExtractor,
            PY_TEST,
            "test_works",
            Known::default(),
        )
    };
    assert!(at("tests/test_app.py").is_empty());
    assert!(at("app/test_app.py").is_empty());
    let rust = kinds_in(
        "crates/x/tests/it.rs",
        &RustExtractor,
        RUST_TEST,
        "works",
        Known::default(),
    );
    assert!(rust.is_empty(), "{rust:?}");
}

#[test]
fn a_rust_test_function_is_entered_by_the_runner_wherever_it_lives() {
    for path in ["src/lib.rs", "crates/x/src/hook.rs"] {
        let found = kinds_in(path, &RustExtractor, RUST_TEST, "works", Known::default());
        assert!(found.is_empty(), "{path}: {found:?}");
    }
}

#[test]
fn a_test_the_runner_is_not_known_to_enter_keeps_its_hedge() {
    let at = |path: &str| {
        kinds_in(
            path,
            &PythonExtractor,
            PY_TEST,
            "test_works",
            Known::default(),
        )
    };
    assert_eq!(at("app/helpers.py"), [Implicit::Test]);
    assert_eq!(at("src/sample"), [Implicit::Test]);
    let kotlin = kinds_in(
        "src/Main.kt",
        &KotlinExtractor,
        "class T {\n    @Test\n    fun works() {}\n}\n",
        "T.works",
        Known::default(),
    );
    assert_eq!(kotlin, [Implicit::Test]);
}

#[test]
fn another_kind_of_a_test_file_definition_keeps_its_hedge() {
    let found = kinds_in(
        "tests/test_app.py",
        &PythonExtractor,
        "class V:\n    def __iter__(self):\n        return self\n",
        "V.__iter__",
        Known::default(),
    );
    assert_eq!(found, [Implicit::Protocol]);
    let entered = Known {
        dispatched: true,
        consumed_outside: false,
    };
    let found = kinds_in(
        "tests/test_app.py",
        &PythonExtractor,
        PY_TEST,
        "test_works",
        entered,
    );
    assert_eq!(found, [Implicit::Override]);
}
