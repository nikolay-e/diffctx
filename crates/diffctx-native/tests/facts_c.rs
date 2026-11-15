//! The C extractor, read through the facts it states: one table per kind of
//! fact, each fixture small enough to read its answer off the source.

use _diffctx::facts::lang::c::CExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, Import, ImportKind, LocalKey,
    LocalKind, Namespace, Receiver, Ref, Scope, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget};

fn c(source: &str) -> BlobFacts {
    CExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    match &local.disambig {
        Some(Disambig::Overload(n)) => text.push_str(&format!("#{n}")),
        Some(Disambig::Cfg(cond)) => text.push_str(&format!("[{cond}]")),
        _ => {}
    }
    if matches!(local.ns, Namespace::Type) {
        text.push_str("@type");
    }
    text
}

fn def_named<'a>(facts: &'a BlobFacts, wanted: &str) -> &'a Def {
    facts
        .defs()
        .find(|def| key(&def.local) == wanted)
        .unwrap_or_else(|| {
            let all: Vec<String> = facts.defs().map(|d| key(&d.local)).collect();
            panic!("no definition {wanted} in {all:?}")
        })
}

fn imports(facts: &BlobFacts) -> Vec<&Import> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Import(import) => Some(import),
            _ => None,
        })
        .collect()
}

fn references<'a>(facts: &'a BlobFacts, name: &str) -> Vec<&'a Ref> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Reference(r) if r.name.as_str() == name => Some(r),
            _ => None,
        })
        .collect()
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => key(local),
        Scope::Callback { .. } => "callback".to_string(),
    }
}

/// `kind receiver [qualifier] in scope`, for the references to `name`.
fn uses(facts: &BlobFacts, name: &str) -> Vec<String> {
    references(facts, name)
        .iter()
        .map(|r| {
            let qualifier: Vec<&str> = r.qualifier.iter().map(|n| n.as_str()).collect();
            let receiver = match &r.receiver {
                Receiver::None => "none".to_string(),
                Receiver::SelfType => "self".to_string(),
                Receiver::Unknown => "unknown".to_string(),
                Receiver::Typed(t) => format!("typed {}", t.as_str()),
                Receiver::New(t) => format!("new {}", t.as_str()),
                Receiver::Module(m) => format!("module {}", m.as_str()),
                Receiver::CallResult(path) => {
                    let path: Vec<&str> = path.iter().map(|n| n.as_str()).collect();
                    format!("call-result {}", path.join("."))
                }
            };
            format!(
                "{:?} {receiver} [{}] in {}",
                r.use_kind,
                qualifier.join("."),
                scope(&r.enclosing)
            )
        })
        .collect()
}

const UNIT: &str = r#"#include <stdio.h>
#include "util.h"

#define MAX 10
#define SQ(x) ((x) * helper(x))

typedef struct node {
    int value;
    struct node *next;
    void (*cb)(int);
} Node;

static int counter;
extern int shared;

int helper(int);

static int twice(int x)
{
    return x * 2;
}

int run(Node *n, int k)
{
    int v = twice(k);
    n->cb(v);
    n->next->value = MAX;
    return helper(v);
}
"#;

#[test]
fn includes_are_imports_of_the_whole_file() {
    let facts = c(UNIT);
    let found: Vec<(String, u32, ImportKind)> = imports(&facts)
        .iter()
        .map(|i| (i.module.as_str().to_string(), i.relative, i.kind))
        .collect();
    assert_eq!(
        found,
        vec![
            ("stdio.h".to_string(), 0, ImportKind::Require),
            ("util.h".to_string(), 1, ImportKind::Require),
        ]
    );
    assert!(imports(&facts).iter().all(|i| i.star.is_star()));
}

#[test]
fn definitions_carry_their_kind_container_and_header() {
    let facts = c(UNIT);
    let sig = |wanted: &str| {
        def_named(&facts, wanted)
            .signature
            .clone()
            .expect("a signature")
    };
    assert_eq!(def_named(&facts, "MAX").kind, DefKind::Macro);
    assert_eq!(sig("MAX"), "#define MAX 10");
    assert_eq!(sig("SQ"), "#define SQ(x) ((x) * helper(x))");
    assert_eq!(def_named(&facts, "node@type").kind, DefKind::Struct);
    assert_eq!(def_named(&facts, "Node@type").kind, DefKind::TypeAlias);
    assert_eq!(def_named(&facts, "node.value").kind, DefKind::Field);
    assert_eq!(sig("node.value"), "int value");
    assert_eq!(sig("node.next"), "struct node *next");
    assert_eq!(sig("node.cb"), "void (*cb)(int)");
    let counter = def_named(&facts, "counter");
    assert_eq!(
        (counter.kind, counter.visibility),
        (DefKind::Variable, Visibility::Private)
    );
    assert_eq!(def_named(&facts, "shared").body_hash, 0);
    let prototype = def_named(&facts, "helper");
    assert_eq!(
        (prototype.kind, prototype.body_hash),
        (DefKind::Function, 0)
    );
    assert_eq!(sig("helper"), "int helper(int)");
    let twice = def_named(&facts, "twice");
    assert_eq!(twice.visibility, Visibility::Private);
    assert_ne!(twice.body_hash, 0);
    assert_eq!(sig("twice"), "static int twice(int x)");
    assert_eq!(def_named(&facts, "run").visibility, Visibility::Public);
}

#[test]
fn references_name_the_chain_before_them() {
    let facts = c(UNIT);
    assert_eq!(uses(&facts, "twice"), ["Call none [] in run"]);
    assert_eq!(uses(&facts, "cb"), ["Call typed Node [] in run"]);
    assert_eq!(uses(&facts, "value"), ["Write typed Node [next] in run"]);
    assert_eq!(uses(&facts, "MAX"), ["Read none [] in run"]);
    assert_eq!(
        uses(&facts, "helper"),
        ["Call none [] in SQ", "Call none [] in run"]
    );
    assert_eq!(uses(&facts, "Node"), ["TypeUse none [] in run"]);
}

#[test]
fn parameters_and_locals_are_no_references() {
    let facts = c(UNIT);
    let locals: Vec<(&str, LocalKind)> = facts
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::LocalBinding(l) => Some((l.name.as_str(), l.kind)),
            _ => None,
        })
        .collect();
    assert!(locals.contains(&("n", LocalKind::Parameter)));
    assert!(locals.contains(&("k", LocalKind::Parameter)));
    assert!(locals.contains(&("v", LocalKind::Variable)));
    assert!(references(&facts, "k").is_empty() && references(&facts, "v").is_empty());
}

#[test]
fn a_header_guard_is_no_condition_but_an_if_is() {
    let guarded = c("#ifndef UTIL_H\n#define UTIL_H\nint helper(int);\n#endif\n");
    assert_eq!(def_named(&guarded, "helper").kind, DefKind::Function);
    assert_eq!(def_named(&guarded, "UTIL_H").kind, DefKind::Macro);
    let split = c(
        "#ifdef WIN\nint plat(void) { return 1; }\n#else\nint plat(void) { return 2; }\n#endif\n",
    );
    let keys: Vec<String> = split.defs().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["plat[#ifdef WIN]", "plat[else #else]"]);
}

#[test]
fn a_prototype_beside_its_definition_is_a_twin() {
    let facts = c("int f(int);\nint f(int x) { return x; }\n");
    let keys: Vec<(String, u64)> = facts.defs().map(|d| (key(&d.local), d.body_hash)).collect();
    assert_eq!(keys.len(), 2);
    assert!(keys[0].0 == "f[declaration]" && keys[0].1 == 0);
    assert!(keys[1].0 == "f" && keys[1].1 != 0);
}

#[test]
fn a_cpp_header_is_read_as_cpp() {
    let facts = c("class A {\npublic:\n  int f();\n};\n");
    assert_eq!(def_named(&facts, "A@type").kind, DefKind::Class);
    assert_eq!(def_named(&facts, "A.f").kind, DefKind::Method);
    assert!(facts.losses.is_empty());
}

#[test]
fn macro_bodies_reference_what_they_name() {
    let facts = c("#define CAT(a, b) a##b\n#define LOG(x) log_impl(x, __LINE__)\n");
    assert_eq!(uses(&facts, "log_impl"), ["Call none [] in LOG"]);
    assert_eq!(uses(&facts, "__LINE__"), ["Read none [] in LOG"]);
    assert!(references(&facts, "a").is_empty() && references(&facts, "b").is_empty());
    assert!(references(&facts, "x").is_empty());
}

#[test]
fn enums_and_anonymous_members() {
    let facts = c(
        "enum color { RED, GREEN = 2 };\nenum { LOCAL };\nstruct s { union { int a; float b; }; int c; };\n",
    );
    assert_eq!(def_named(&facts, "color@type").kind, DefKind::Enum);
    assert_eq!(def_named(&facts, "color.RED").kind, DefKind::EnumMember);
    assert_eq!(def_named(&facts, "color.GREEN").kind, DefKind::EnumMember);
    assert_eq!(def_named(&facts, "LOCAL").kind, DefKind::EnumMember);
    for field in ["s.a", "s.b", "s.c"] {
        assert_eq!(def_named(&facts, field).kind, DefKind::Field);
    }
}

#[test]
fn a_call_through_a_local_pointer_is_unknown() {
    let facts = c("void run(void (*cb)(int), int x) { cb(x); }\n");
    assert_eq!(uses(&facts, "cb"), ["Call unknown [] in run"]);
}

#[test]
fn main_is_an_entry_point() {
    let facts = c("int main(void) { return 0; }\n");
    let mains: Vec<String> = facts
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::EntryPoint(e) if e.kind == EntryKind::Main => Some(scope(&e.target)),
            _ => None,
        })
        .collect();
    assert_eq!(mains, ["main"]);
}

fn parse_error_lines(facts: &BlobFacts) -> Vec<Vec<(u32, u32)>> {
    facts
        .losses
        .iter()
        .filter_map(|l| match l {
            Loss::ParseError { lines, .. } => Some(lines.clone()),
            _ => None,
        })
        .collect()
}

/// A missing `}` and a broken signature make the grammar replace the whole
/// file with an error node. `keep`, whole inside it, is read with its doc
/// comment, and only the lines of what the walk could not read are the
/// parse error: the stray tokens of `helper` (1-2) and the broken `tail`
/// (9-11). Red on the base: the error node was unread whole, lines 1-11.
#[test]
fn an_item_under_an_error_node_is_read_and_only_the_rest_is_unread() {
    let facts = c(
        "int helper(int x) {\n    return x + 1;\n\n/* Keeps the count. */\nint keep(void) {\n    return helper(1);\n}\n\nint tail( { int x) {\n    return other(x);\n}\n",
    );
    def_named(&facts, "keep");
    assert_eq!(parse_error_lines(&facts), [[(1, 2), (9, 11)]]);
}

/// A declarator chain the walk gives up on (past 32 links) is a loss, not a
/// declaration quietly left out. Red on the base: `declared` returned `None`
/// and the file lost `x` with no loss.
#[test]
fn a_declarator_chain_past_the_cut_is_a_loss() {
    let deep = c(&format!("int {}x;\n", "*".repeat(40)));
    assert!(
        deep.losses
            .iter()
            .any(|l| matches!(l, Loss::ExtractorFailed { .. })),
        "{:?}",
        deep.losses
    );
    let shallow = c(&format!("int {}x;\n", "*".repeat(20)));
    def_named(&shallow, "x");
    assert!(shallow.losses.is_empty(), "{:?}", shallow.losses);
}
