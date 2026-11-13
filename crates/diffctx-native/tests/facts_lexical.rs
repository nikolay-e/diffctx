//! The comment and string ranges every extractor records beside its facts, read
//! off real snippets of several families with the real grammars: a needle is a
//! word of the snippet and the answer is what its first and last byte are.

mod facts_fixture;
mod snapshot_fixture;

use _diffctx::facts::lang::c::CExtractor;
use _diffctx::facts::lang::go::GoExtractor;
use _diffctx::facts::lang::java::JavaExtractor;
use _diffctx::facts::lang::kotlin::KotlinExtractor;
use _diffctx::facts::lang::php::PhpExtractor;
use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::lang::shell::ShellExtractor;
use _diffctx::facts::lang::swift::SwiftExtractor;
use _diffctx::facts::lang::typescript::JavaScript;
use _diffctx::facts::{Extractor, FactsCache, Lexical, facts_of};
use _diffctx::snapshot::{RepoPath, RunBudget, SideTag};
use facts_fixture::review;
use snapshot_fixture::Repo;

const COMMENT: Option<Lexical> = Some(Lexical::Comment);
const STRING: Option<Lexical> = Some(Lexical::String);
const CODE: Option<Lexical> = None;

fn read(extractor: &dyn Extractor, source: &str, expected: &[(&str, Option<Lexical>)]) {
    let blob = extractor.extract(source.as_bytes(), &RunBudget::default());
    for pair in blob.lexical.windows(2) {
        assert!(
            pair[0].0.end <= pair[1].0.start,
            "sorted and apart: {pair:?}"
        );
    }
    for (needle, want) in expected {
        let from = source.find(needle).expect("the needle is in the snippet");
        let last = from + needle.len() - 1;
        assert_eq!(
            blob.lexical_at(from as u32),
            *want,
            "first byte of {needle}"
        );
        assert_eq!(blob.lexical_at(last as u32), *want, "last byte of {needle}");
    }
}

#[test]
fn python_fstring_replacement_fields_are_code_and_nested_strings_are_strings() {
    let source = "# py_note\ndef f(d):\n    msg = f\"lit_a {inner_one} lit_b {d['key_nested']}\"\n    return msg\n";
    read(
        &PythonExtractor,
        source,
        &[
            ("py_note", COMMENT),
            ("lit_a", STRING),
            ("{inner_one}", CODE),
            ("lit_b", STRING),
            ("key_nested", STRING),
            ("msg", CODE),
        ],
    );
}

#[test]
fn javascript_template_substitutions_are_code() {
    let source = "// js_note\n/* js_block */\nconst a = `tpl_text ${inner_js} tpl_more`;\nconst b = \"plain_js\";\n";
    read(
        &JavaScript,
        source,
        &[
            ("js_note", COMMENT),
            ("js_block", COMMENT),
            ("tpl_text", STRING),
            ("${inner_js}", CODE),
            ("tpl_more", STRING),
            ("plain_js", STRING),
            ("const", CODE),
        ],
    );
}

#[test]
fn rust_raw_strings_are_strings_and_literals_in_a_macro_are_code() {
    let source = "// rs_note\n/// rs_doc\nfn f() {\n    let raw = r#\"raw_text\"#;\n    println!(\"fmt_in_macro {name}\");\n    let s = \"plain_rs\";\n}\n";
    read(
        &RustExtractor,
        source,
        &[
            ("rs_note", COMMENT),
            ("rs_doc", COMMENT),
            ("r#\"raw_text\"#", STRING),
            ("fmt_in_macro", CODE),
            ("plain_rs", STRING),
            ("let", CODE),
        ],
    );
}

#[test]
fn kotlin_interpolations_are_code() {
    let source = "// kt_note\nval a = \"kt_lit ${kt_expr} kt_end $kt_short\"\n";
    read(
        &KotlinExtractor,
        source,
        &[
            ("kt_note", COMMENT),
            ("kt_lit", STRING),
            ("${kt_expr}", CODE),
            ("kt_end", STRING),
            ("kt_short", CODE),
            ("val", CODE),
        ],
    );
}

#[test]
fn php_double_quoted_variables_are_code_and_single_quoted_strings_are_not() {
    let source = "<?php\n// php_note\n# php_hash\n$a = \"php_dq $php_var {$php_obj->prop} php_tail\";\n$b = 'php_single';\n";
    read(
        &PhpExtractor,
        source,
        &[
            ("php_note", COMMENT),
            ("php_hash", COMMENT),
            ("php_dq", STRING),
            ("php_var", CODE),
            ("php_obj", CODE),
            ("php_tail", STRING),
            ("php_single", STRING),
        ],
    );
}

#[test]
fn swift_interpolations_are_code() {
    let source = "// sw_note\nlet a = \"sw_lit \\(sw_expr) sw_end\"\n";
    read(
        &SwiftExtractor,
        source,
        &[
            ("sw_note", COMMENT),
            ("sw_lit", STRING),
            ("sw_expr", CODE),
            ("sw_end", STRING),
            ("let", CODE),
        ],
    );
}

#[test]
fn go_c_and_java_record_comments_strings_and_char_literals() {
    read(
        &GoExtractor,
        "// go_note\npackage p\nvar s = `go_raw`\nvar t = \"go_str\"\n",
        &[
            ("go_note", COMMENT),
            ("go_raw", STRING),
            ("go_str", STRING),
            ("package", CODE),
        ],
    );
    read(
        &CExtractor,
        "// c_note\n/* c_block */\nconst char *s = \"c_lit\";\nchar k = 'q';\n",
        &[
            ("c_note", COMMENT),
            ("c_block", COMMENT),
            ("c_lit", STRING),
            ("'q'", STRING),
            ("const", CODE),
        ],
    );
    read(
        &JavaExtractor,
        "// jv_note\nclass A { String s = \"jv_lit\"; }\n",
        &[("jv_note", COMMENT), ("jv_lit", STRING), ("class", CODE)],
    );
}

#[test]
fn shell_keeps_only_its_comments_out_of_code() {
    read(
        &ShellExtractor,
        "# sh_note\necho \"sh_word $sh_var\"\n",
        &[("sh_note", COMMENT), ("sh_word", CODE), ("sh_var", CODE)],
    );
}

#[test]
fn the_ranges_survive_the_disk_cache() {
    let dir = tempfile::tempdir().expect("dir");
    let repo = Repo::init();
    repo.write("m.py", "# note\nname = 'text'\n");
    let head = repo.commit("only");
    let ctx = review(&repo, &head, &head);
    let path = RepoPath::new("m.py").expect("a path");
    let facts = |cache: &FactsCache| {
        facts_of(&ctx, SideTag::Head, cache, &path)
            .expect("facts")
            .expect("some")
    };

    let first = FactsCache::new(dir.path().to_path_buf());
    let written = facts(&first);
    assert_eq!(written.blob.lexical.len(), 2);

    let fresh_process = FactsCache::new(dir.path().to_path_buf());
    let served = facts(&fresh_process);
    assert_eq!(fresh_process.stats().disk_hits, 1);
    assert_eq!(served.blob.lexical, written.blob.lexical);
}
