//! The Kotlin extractor, read through the facts it states: one table per kind of
//! fact, each fixture small enough to read its answer off the source.

use _diffctx::facts::lang::kotlin::KotlinExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, Import, ImportKind, LocalKey,
    LocalKind, Namespace, Receiver, Ref, Scope, UseKind, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget};

fn kt(source: &str) -> BlobFacts {
    KotlinExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    match &local.disambig {
        Some(Disambig::Overload(n)) => text.push_str(&format!("#{n}")),
        Some(Disambig::ImplOf(receiver)) => text.push_str(&format!("#ext:{}", receiver.as_str())),
        Some(other) => text.push_str(&format!("#{other:?}")),
        None => {}
    }
    if matches!(local.ns, Namespace::Type) {
        text.push_str("@type");
    }
    text
}

fn definitions(facts: &BlobFacts) -> Vec<&Def> {
    facts.defs().collect()
}

fn defs(facts: &BlobFacts) -> Vec<String> {
    definitions(facts)
        .iter()
        .map(|def| format!("{} {:?}", key(&def.local), def.kind))
        .collect()
}

fn def_named<'a>(facts: &'a BlobFacts, wanted: &str) -> &'a Def {
    definitions(facts)
        .into_iter()
        .find(|def| key(&def.local) == wanted)
        .unwrap_or_else(|| panic!("no definition {wanted} in {:?}", defs(facts)))
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

fn receiver(receiver: &Receiver) -> String {
    match receiver {
        Receiver::None => "none".to_string(),
        Receiver::SelfType => "self".to_string(),
        Receiver::Unknown => "unknown".to_string(),
        Receiver::Typed(t) => format!("typed {}", t.as_str()),
        Receiver::CallResult(path) => {
            let path: Vec<&str> = path.iter().map(|n| n.as_str()).collect();
            format!("call-result {}", path.join("."))
        }
        other => format!("{other:?}"),
    }
}

/// `kind receiver [qualifier] in scope`, for the references to `name`.
fn uses(facts: &BlobFacts, name: &str) -> Vec<String> {
    references(facts, name)
        .iter()
        .map(|r| {
            let qualifier: Vec<&str> = r.qualifier.iter().map(|n| n.as_str()).collect();
            format!(
                "{:?} {} [{}] in {}",
                r.use_kind,
                receiver(&r.receiver),
                qualifier.join("."),
                scope(&r.enclosing)
            )
        })
        .collect()
}

const SHOP: &str = "package com.acme.shop

import com.acme.util.slug as makeSlug
import com.acme.core.*
import com.acme.core.Outer.Inner

class Cart(private val repo: Repo, plain: Int) : Base(), Repo {
    var total: Int = 0

    fun add(item: Item): Int {
        val saved = repo.save(item)
        total += saved
        this.total = saved
        return makeSlug(item.name).length
    }

    fun add(item: Item, qty: Int): Int = qty

    override fun find(id: Int): String = \"row\"

    companion object {
        const val MAX = 3
        fun empty() = Cart(Repo(), 0)
    }
}

interface Repo {
    fun find(id: Int): String
    fun save(item: Item): Int
}

object Registry {
    val default = Cart(Repo(), 1)
}

fun String.pad(): String = this
val Cart.size: Int get() = 1
fun all(items: List<Item>) = items.map(::price)
";

#[test]
fn the_package_and_the_imports_are_stated_as_written() {
    let facts = kt(SHOP);
    assert!(facts.losses.is_empty(), "{:?}", facts.losses);
    let all = imports(&facts);
    let package = all
        .iter()
        .find(|i| i.kind == ImportKind::SideEffect)
        .expect("the package header");
    assert_eq!(package.module.as_str(), "com.acme.shop");
    let aliased = all
        .iter()
        .find(|i| i.module.as_str() == "com.acme.util")
        .expect("the aliased import");
    assert_eq!(aliased.kind, ImportKind::Static);
    assert_eq!(aliased.names.len(), 1);
    assert_eq!(aliased.names[0].0.as_str(), "slug");
    assert_eq!(
        aliased.names[0].1.as_ref().map(|a| a.as_str()),
        Some("makeSlug")
    );
    let star = all
        .iter()
        .find(|i| i.module.as_str() == "com.acme.core" && i.star.is_star())
        .expect("the wildcard import");
    assert!(star.names.is_empty());
    let nested = all
        .iter()
        .find(|i| i.module.as_str() == "com.acme.core.Outer")
        .expect("an import of a nested type names its holder as the module");
    assert_eq!(nested.names[0].0.as_str(), "Inner");
}

#[test]
fn a_file_without_a_package_has_none() {
    let facts = kt("fun main() {}\n");
    assert!(imports(&facts).is_empty());
}

#[test]
fn members_extensions_companions_and_overloads_are_definitions_with_their_containers() {
    let facts = kt(SHOP);
    let found = defs(&facts);
    for expected in [
        "Cart@type Class",
        "Cart.repo Property",
        "Cart.total Property",
        "Cart.add Method",
        "Cart.add#1 Method",
        "Cart.find Method",
        "Cart.Companion@type Object",
        "Cart.Companion.MAX Property",
        "Cart.Companion.empty Method",
        "Repo@type Interface",
        "Repo.find Method",
        "Repo.save Method",
        "Registry@type Object",
        "Registry.default Property",
        "String.pad#ext:String Function",
        "Cart.size#ext:Cart Property",
        "all Function",
    ] {
        assert!(
            found.contains(&expected.to_string()),
            "{expected}: {found:?}"
        );
    }
    assert!(
        !found.iter().any(|d| d.contains("plain")),
        "a plain constructor parameter is not a property: {found:?}"
    );
    assert!(
        !found.iter().any(|d| d.contains("saved")),
        "a local is no definition: {found:?}"
    );
}

#[test]
fn the_header_is_the_signature_and_a_plain_call_types_a_value_without_a_written_type() {
    let facts = kt(SHOP);
    let signature = |name: &str| def_named(&facts, name).signature.clone().unwrap();
    assert_eq!(signature("Cart.repo"), "private val repo: Repo");
    assert_eq!(signature("Cart.add"), "fun add(item: Item): Int");
    assert_eq!(
        signature("Cart@type"),
        "class Cart(private val repo: Repo, plain: Int) : Base(), Repo"
    );
    assert_eq!(signature("Registry.default"), "val default = Cart()");
    assert_eq!(signature("Cart.Companion.empty"), "fun empty() = Cart()");
    assert_eq!(signature("Cart.size#ext:Cart"), "val Cart.size: Int");
}

#[test]
fn visibility_follows_the_modifier_and_the_owners() {
    let facts = kt(
        "internal class A {\n    fun open() {}\n    private fun hidden() {}\n    protected fun kept() {}\n}\nclass B { fun shown() {} }\n",
    );
    let visibility = |name: &str| def_named(&facts, name).visibility;
    assert_eq!(visibility("A.open"), Visibility::Crate);
    assert_eq!(visibility("A.hidden"), Visibility::Private);
    assert_eq!(visibility("A.kept"), Visibility::Protected);
    assert_eq!(visibility("B.shown"), Visibility::Public);
}

#[test]
fn a_reference_names_the_last_segment_and_the_chain_before_it() {
    let facts = kt(SHOP);
    assert_eq!(
        uses(&facts, "save"),
        ["Call none [repo] in Cart.add"],
        "a name that is no local is left to the resolver"
    );
    assert_eq!(
        uses(&facts, "name"),
        ["Read typed Item [item] in Cart.add"],
        "a parameter with a written type types its chain"
    );
    assert_eq!(
        uses(&facts, "length"),
        ["Read call-result makeSlug [] in Cart.add"]
    );
    let total = uses(&facts, "total");
    assert!(
        total.contains(&"Write none [] in Cart.add".to_string()),
        "+= writes: {total:?}"
    );
    assert!(
        total.contains(&"Write self [] in Cart.add".to_string()),
        "this.total = writes: {total:?}"
    );
    assert!(
        uses(&facts, "price").contains(&"PassAsValue none [] in all".to_string()),
        "{:?}",
        uses(&facts, "price")
    );
    assert!(
        uses(&facts, "Item")
            .iter()
            .any(|u| u.starts_with("TypeUse none [] in Cart.add")),
        "{:?}",
        uses(&facts, "Item")
    );
    assert!(
        uses(&facts, "makeSlug").contains(&"Call none [] in Cart.add".to_string()),
        "the alias is the name as written"
    );
}

#[test]
fn a_local_initialised_by_a_call_types_its_chain_by_that_call() {
    let facts = kt(
        "fun run() {\n    val repo = Repo()\n    val other = make()\n    val client = factory.build()\n    repo.save()\n    other.go()\n    client.send()\n    unknown.poke()\n}\n",
    );
    assert_eq!(
        uses(&facts, "save"),
        ["Call call-result Repo [repo] in run"]
    );
    assert_eq!(uses(&facts, "go"), ["Call call-result make [other] in run"]);
    assert_eq!(
        uses(&facts, "send"),
        ["Call call-result factory.build [client] in run"]
    );
    assert_eq!(uses(&facts, "poke"), ["Call none [unknown] in run"]);
}

#[test]
fn a_binding_is_visible_from_its_end_to_the_end_of_its_scope() {
    let facts = kt("fun f(a: Int) {\n    val x = a\n    use(x)\n}\n");
    let locals: Vec<(&str, LocalKind)> = facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::LocalBinding(l) => Some((l.name.as_str(), l.kind)),
            _ => None,
        })
        .collect();
    assert!(locals.contains(&("a", LocalKind::Parameter)), "{locals:?}");
    assert!(locals.contains(&("x", LocalKind::Variable)), "{locals:?}");
}

#[test]
fn an_override_references_its_own_name_from_itself() {
    let facts = kt(SHOP);
    let found: Vec<(String, String)> = references(&facts, "find")
        .iter()
        .filter(|r| r.use_kind == UseKind::Override)
        .map(|r| (scope(&r.enclosing), r.name.to_string()))
        .collect();
    assert_eq!(found, [("Cart.find".to_string(), "find".to_string())]);
}

#[test]
fn tests_and_main_are_entry_points() {
    let facts = kt(
        "class T {\n    @Test\n    fun works() {}\n    fun helper() {}\n}\nfun main(args: Array<String>) {}\nfun other() {}\n",
    );
    let entries: Vec<(EntryKind, String)> = facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::EntryPoint(e) => Some((e.kind, scope(&e.target))),
            _ => None,
        })
        .collect();
    assert_eq!(
        entries,
        [
            (EntryKind::Test, "T.works".to_string()),
            (EntryKind::Main, "main".to_string())
        ]
    );
}

#[test]
fn a_member_changes_its_own_hashes_and_not_its_class() {
    let class = |body: &str| {
        format!(
            "class A {{\n    fun one(): Int {{\n        return {body}\n    }}\n\n    fun two(): Int = 2\n}}\n"
        )
    };
    let before = kt(&class("1"));
    let after = kt(&class("3"));
    let one = |facts: &BlobFacts| {
        let def = def_named(facts, "A.one");
        (def.body_hash, def.text_hash)
    };
    assert_ne!(one(&before), one(&after));
    assert_eq!(
        def_named(&before, "A.two").body_hash,
        def_named(&after, "A.two").body_hash
    );
    assert_eq!(
        def_named(&before, "A@type").body_hash,
        def_named(&after, "A@type").body_hash,
        "the class's body leaves its members out"
    );
    let relaid = kt(&class("1").replace("        return", "      return"));
    assert_eq!(
        def_named(&before, "A.one").body_hash,
        def_named(&relaid, "A.one").body_hash,
        "indentation is not the tree"
    );
    assert_ne!(
        def_named(&before, "A.one").text_hash,
        def_named(&relaid, "A.one").text_hash
    );
    assert_eq!(def_named(&before, "A@type").visibility, Visibility::Public);
    assert_eq!(def_named(&before, "A.two").kind, DefKind::Method);
}

#[test]
fn a_doc_comment_is_hashed_by_its_words() {
    let with = |doc: &str| kt(&format!("{doc}\nfun f() = 1\n"));
    let a = with("/**\n * Adds   one.\n */");
    let b = with("/** Adds one. */");
    let c = with("/** Adds two. */");
    let doc = |facts: &BlobFacts| def_named(facts, "f").doc_hash;
    assert!(doc(&a).is_some());
    assert_eq!(doc(&a), doc(&b));
    assert_ne!(doc(&a), doc(&c));
    assert_eq!(doc(&kt("fun f() = 1\n")), None);
}

#[test]
fn a_parse_error_is_a_loss_and_the_rest_survives() {
    let facts = kt("class A {\n    fun ok() = 1\n}\nclass B {\n    fun broken(: Int {\n}\n");
    assert!(
        facts
            .losses
            .iter()
            .any(|loss| matches!(loss, Loss::ParseError { .. })),
        "{:?}",
        facts.losses
    );
    assert!(defs(&facts).contains(&"A.ok Method".to_string()));
}

#[test]
fn facts_come_in_source_order() {
    let facts = kt(SHOP);
    let starts: Vec<u32> = facts.facts.iter().map(|f| f.site().bytes.start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
}
