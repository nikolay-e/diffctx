//! The PHP extractor, read through the facts it states: one table per kind of
//! fact, each fixture small enough to read its answer off the source.

use _diffctx::facts::lang::php::PhpExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, Import, ImportKind, LocalKey,
    LocalKind, Namespace, Receiver, Ref, Scope, Visibility,
};
use _diffctx::snapshot::RunBudget;

fn php(source: &str) -> BlobFacts {
    PhpExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    if let Some(Disambig::Overload(n)) = &local.disambig {
        text.push_str(&format!("#{n}"));
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
                other => format!("{other:?}"),
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

const USERS: &str = r#"<?php
namespace App\Service;

use App\Repo\UserRepo;
use App\Util\{Str, Arr as A};
use function App\helpers\slug;
use const App\Consts\MAX;

#[Service]
final class Users extends Base implements Countable
{
    use Loggable;

    public const LIMIT = 5;
    private ?UserRepo $repo;

    public function __construct(private Mailer $mailer, ?UserRepo $repo = null)
    {
        $this->repo = $repo;
    }

    public function find(int $id): ?User
    {
        $this->mailer->send($id);
        return $this->repo->get($id);
    }
}
"#;

#[test]
fn a_namespace_and_the_uses_are_imports() {
    let facts = php(USERS);
    let all: Vec<(String, ImportKind, Vec<String>)> = imports(&facts)
        .iter()
        .map(|i| {
            (
                i.module.as_str().to_string(),
                i.kind,
                i.names
                    .iter()
                    .map(|(n, alias)| match alias {
                        Some(a) => format!("{} as {}", n.as_str(), a.as_str()),
                        None => n.as_str().to_string(),
                    })
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        all,
        vec![
            ("App\\Service".into(), ImportKind::SideEffect, vec![]),
            (
                "App\\Repo".into(),
                ImportKind::Static,
                vec!["UserRepo".into()]
            ),
            ("App\\Util".into(), ImportKind::Static, vec!["Str".into()]),
            (
                "App\\Util".into(),
                ImportKind::Static,
                vec!["Arr as A".into()]
            ),
            (
                "App\\helpers".into(),
                ImportKind::FunctionOnly,
                vec!["slug".into()]
            ),
            (
                "App\\Consts".into(),
                ImportKind::ConstOnly,
                vec!["MAX".into()]
            ),
        ]
    );
}

#[test]
fn a_flat_namespace_is_in_effect_until_the_next() {
    let source = "<?php\nnamespace A;\nclass X {}\nnamespace B;\nclass Y {}\n";
    let facts = php(source);
    let spaces: Vec<(&str, &str)> = imports(&facts)
        .iter()
        .map(|i| {
            (
                i.module.as_str(),
                &source[i.site.bytes.start as usize..i.site.bytes.end as usize],
            )
        })
        .collect();
    assert_eq!(spaces.len(), 2);
    assert!(spaces[0].1.contains("class X") && !spaces[0].1.contains("class Y"));
    assert!(spaces[1].1.contains("class Y"));
}

#[test]
fn definitions_carry_their_kind_container_and_header() {
    let facts = php(USERS);
    let class = def_named(&facts, "Users@type");
    assert_eq!(class.kind, DefKind::Class);
    assert_eq!(
        class.signature.as_deref(),
        Some("final class Users extends Base implements Countable use Loggable")
    );
    assert_eq!(class.decorators[0].as_str(), "Service");
    assert_eq!(
        def_named(&facts, "Users.__construct").kind,
        DefKind::Constructor
    );
    let find = def_named(&facts, "Users.find");
    assert_eq!(find.kind, DefKind::Method);
    assert_eq!(
        find.signature.as_deref(),
        Some("public function find(int $id): ?User")
    );
    let limit = def_named(&facts, "Users.LIMIT");
    assert_eq!(limit.kind, DefKind::Constant);
    let repo = def_named(&facts, "Users.repo");
    assert_eq!(
        (repo.kind, repo.visibility),
        (DefKind::Property, Visibility::Private)
    );
    assert_eq!(repo.signature.as_deref(), Some("private ?UserRepo $repo"));
    let promoted = def_named(&facts, "Users.mailer");
    assert_eq!(promoted.kind, DefKind::Property);
    assert_eq!(
        promoted.signature.as_deref(),
        Some("private Mailer $mailer")
    );
}

#[test]
fn types_traits_and_attributes_are_uses_of_their_owner() {
    let facts = php(USERS);
    assert_eq!(uses(&facts, "Base"), ["TypeUse none [] in Users@type"]);
    assert_eq!(uses(&facts, "Loggable"), ["TypeUse none [] in Users@type"]);
    assert_eq!(uses(&facts, "Service"), ["Decorate none [] in Users@type"]);
    assert_eq!(
        uses(&facts, "Mailer"),
        ["TypeUse none [] in Users.__construct"]
    );
    assert_eq!(uses(&facts, "User"), ["TypeUse none [] in Users.find"]);
}

#[test]
fn a_member_access_names_the_chain_before_it() {
    let facts = php(USERS);
    assert_eq!(uses(&facts, "send"), ["Call self [mailer] in Users.find"]);
    assert_eq!(uses(&facts, "get"), ["Call self [repo] in Users.find"]);
    // `$this->repo->get()` reads `repo` on the way to `get`.
    assert_eq!(
        uses(&facts, "repo"),
        [
            "Write self [] in Users.__construct",
            "Read self [] in Users.find"
        ]
    );
}

#[test]
fn parameters_are_locals() {
    let facts = php(USERS);
    let params: Vec<&str> = facts
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::LocalBinding(l) if l.kind == LocalKind::Parameter => Some(l.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(params, ["$mailer", "$repo", "$id"]);
}

#[test]
fn receivers_are_what_the_code_states() {
    let facts = php(r#"<?php
function run(Svc $s, ?Other $o, A|B $u, int $n)
{
    $x = new Repo();
    $x->save();
    $s->go();
    $o->stop();
    $u->any();
    $m = 'go';
    $s->$m();
    $c = 'Repo';
    new $c();
    parent::boot();
    self::make();
    \Lib\Util::help();
    Foo\bar();
    $f = strlen(...);
    $y = build();
    $y->done();
}
"#);
    assert_eq!(uses(&facts, "save"), ["Call new Repo [] in run"]);
    assert_eq!(uses(&facts, "go"), ["Call typed Svc [] in run"]);
    assert_eq!(uses(&facts, "stop"), ["Call typed Other [] in run"]);
    assert_eq!(uses(&facts, "any"), ["Call unknown [] in run"]);
    assert_eq!(uses(&facts, "$m"), ["Call typed Svc [] in run"]);
    assert_eq!(uses(&facts, "$c"), ["Instantiate unknown [] in run"]);
    assert_eq!(uses(&facts, "boot"), ["Call none [parent] in run"]);
    assert_eq!(uses(&facts, "make"), ["Call none [self] in run"]);
    assert_eq!(uses(&facts, "help"), ["Call none [.Lib.Util] in run"]);
    assert_eq!(uses(&facts, "bar"), ["Call module Foo [Foo] in run"]);
    assert_eq!(uses(&facts, "strlen"), ["PassAsValue none [] in run"]);
    assert_eq!(uses(&facts, "done"), ["Call call-result build [] in run"]);
}

#[test]
fn a_variable_assigned_two_ways_is_untyped() {
    let facts = php(
        "<?php\nfunction f($a) {\n    $x = new One();\n    if ($a) { $x = new Two(); }\n    $x->go();\n}\n",
    );
    assert_eq!(uses(&facts, "go"), ["Call unknown [] in f"]);
}

#[test]
fn tests_overrides_and_anonymous_classes() {
    let facts = php(r#"<?php
class UsersTest extends TestCase
{
    public function testFind() {}
    #[Test] public function other() {}
    public function helper() {}
}
interface Shape { public function area(): float; }
class Sq implements Shape { public function area(): float { return 1.0; } }
$o = new class extends Base { public function run() {} };
"#);
    let tests: Vec<String> = facts
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::EntryPoint(e) if e.kind == EntryKind::Test => Some(scope(&e.target)),
            _ => None,
        })
        .collect();
    assert_eq!(tests, ["UsersTest.testFind", "UsersTest.other"]);
    assert_eq!(uses(&facts, "area"), ["Override none [] in Sq.area"]);
    assert_eq!(def_named(&facts, "new Base@type").kind, DefKind::Class);
    assert_eq!(uses(&facts, "run"), ["Override none [] in new Base.run"]);
}

#[test]
fn a_body_edit_moves_the_body_hash_alone() {
    let before = php(
        "<?php\nclass A {\n    public function f() { return 1; }\n    public function g() { return 2; }\n}\n",
    );
    let after = php(
        "<?php\nclass A {\n    public function f() { return 3; }\n    public function g() { return 2; }\n}\n",
    );
    let f = |facts: &BlobFacts| {
        let d = def_named(facts, "A.f");
        (d.signature.clone(), d.body_hash, d.text_hash)
    };
    assert_eq!(f(&before).0, f(&after).0);
    assert_ne!(f(&before).1, f(&after).1);
    let class = |facts: &BlobFacts| {
        let d = def_named(facts, "A@type");
        (d.body_hash, d.text_hash)
    };
    assert_eq!(class(&before), class(&after));
    let g = |facts: &BlobFacts| def_named(facts, "A.g").body_hash;
    assert_eq!(g(&before), g(&after));
}

#[test]
fn a_method_and_a_property_of_one_name_are_twins() {
    let facts = php("<?php\nclass A {\n    private $count;\n    public function count() {}\n}\n");
    assert_eq!(def_named(&facts, "A.count").kind, DefKind::Property);
    assert_eq!(def_named(&facts, "A.count#1").kind, DefKind::Method);
}

#[test]
fn a_literal_require_is_an_import() {
    let facts =
        php("<?php\nrequire_once __DIR__ . '/lib/a.php';\nrequire 'b.php';\ninclude $dynamic;\n");
    let found: Vec<(String, u32, ImportKind)> = imports(&facts)
        .iter()
        .map(|i| (i.module.as_str().to_string(), i.relative, i.kind))
        .collect();
    assert_eq!(
        found,
        vec![
            ("/lib/a.php".to_string(), 1, ImportKind::Require),
            ("b.php".to_string(), 0, ImportKind::Require),
        ]
    );
}

#[test]
fn a_call_written_through_properties_names_the_path() {
    let facts = php(
        "<?php\nfunction f(Svc $s) {\n    $s->repo->find()->name();\n    $s->make()->name();\n}\n",
    );
    let name = uses(&facts, "name");
    assert_eq!(
        name,
        [
            "Call call-result Svc.->.repo.find [] in f",
            "Call call-result Svc.make [] in f"
        ]
    );
}
