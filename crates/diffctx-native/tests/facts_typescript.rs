//! The TypeScript and JavaScript extractor, read through the facts it states:
//! one table per kind of fact, each fixture small enough to read its answer
//! off the source.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::lang::typescript::{JavaScript, TypeScript};
use _diffctx::facts::{
    BlobFacts, Def, Disambig, Export, Extractor, Fact, Import, LocalKey, Namespace, Receiver, Ref,
    Scope, Star, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget};

fn ts(source: &str) -> BlobFacts {
    TypeScript.extract(source.as_bytes(), &RunBudget::default())
}

fn js(source: &str) -> BlobFacts {
    JavaScript.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    match &local.disambig {
        Some(Disambig::Overload(n)) => text.push_str(&format!("#{n}")),
        Some(other) => text.push_str(&format!("#{other:?}")),
        None => {}
    }
    if matches!(local.ns, Namespace::Type) {
        text.push_str("@type");
    }
    text
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => format!("def {}", key(local)),
        Scope::Callback { callee, parent, .. } => {
            let callee: Vec<&str> = callee.iter().map(|n| n.as_str()).collect();
            format!("callback {} in {}", callee.join("."), self::scope(parent))
        }
    }
}

fn receiver(receiver: &Receiver) -> String {
    match receiver {
        Receiver::None => "none".to_string(),
        Receiver::SelfType => "self".to_string(),
        Receiver::Unknown => "unknown".to_string(),
        Receiver::Typed(t) => format!("typed {}", t.as_str()),
        Receiver::New(t) => format!("new {}", t.as_str()),
        Receiver::CallResult(path) => {
            let path: Vec<&str> = path.iter().map(|n| n.as_str()).collect();
            format!("call-result {}", path.join("."))
        }
        other => format!("{other:?}"),
    }
}

fn definitions(facts: &BlobFacts) -> Vec<&Def> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Definition(def) => Some(def),
            _ => None,
        })
        .collect()
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

fn references(facts: &BlobFacts) -> Vec<&Ref> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Reference(reference) => Some(reference),
            _ => None,
        })
        .collect()
}

fn qualified(reference: &Ref) -> String {
    let mut parts: Vec<&str> = reference.qualifier.iter().map(|n| n.as_str()).collect();
    parts.push(reference.name.as_str());
    parts.join(".")
}

fn refs(facts: &BlobFacts) -> Vec<String> {
    references(facts)
        .iter()
        .map(|r| {
            format!(
                "{:?} {} in {}",
                r.use_kind,
                qualified(r),
                scope(&r.enclosing)
            )
        })
        .collect()
}

fn ref_named<'a>(facts: &'a BlobFacts, wanted: &str) -> Vec<&'a Ref> {
    references(facts)
        .into_iter()
        .filter(|r| r.name.as_str() == wanted)
        .collect()
}

fn locals(facts: &BlobFacts) -> Vec<String> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::LocalBinding(local) => Some(format!(
                "{:?} {} in {}",
                local.kind,
                local.name.as_str(),
                scope(&local.enclosing)
            )),
            _ => None,
        })
        .collect()
}

fn imports(facts: &BlobFacts) -> Vec<String> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Import(import) => Some(import_line(import)),
            _ => None,
        })
        .collect()
}

fn import_line(import: &Import) -> String {
    let names: Vec<String> = import
        .names
        .iter()
        .map(|(name, alias)| match alias {
            Some(alias) => format!("{} as {}", name.as_str(), alias.as_str()),
            None => name.as_str().to_string(),
        })
        .collect();
    let star = match &import.star {
        Star::None => "-".to_string(),
        Star::Glob => "*".to_string(),
        Star::Alias(alias) => alias.as_str().to_string(),
    };
    let default = import
        .default
        .as_ref()
        .map_or("-".to_string(), |n| n.as_str().to_string());
    format!(
        "{:?} {} rel={} names=[{}] star={star} default={default} in {}",
        import.kind,
        import.module.as_str(),
        import.relative,
        names.join(", "),
        scope(&import.enclosing)
    )
}

fn exports(facts: &BlobFacts) -> Vec<String> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Export(Export::Named {
                name,
                alias,
                from,
                type_only,
                ..
            }) => Some(format!(
                "Named {}{}{}{}",
                name.as_str(),
                alias
                    .as_ref()
                    .map_or(String::new(), |a| format!(" as {}", a.as_str())),
                from.as_ref()
                    .map_or(String::new(), |m| format!(" from {}", m.as_str())),
                if *type_only { " type" } else { "" }
            )),
            Fact::Export(Export::Star { from, alias, .. }) => Some(format!(
                "Star {}{}",
                from.as_str(),
                alias
                    .as_ref()
                    .map_or(String::new(), |a| format!(" as {}", a.as_str()))
            )),
            Fact::Export(Export::Default { local, .. }) => Some(format!(
                "Default {}",
                local.as_ref().map_or("-", |n| n.as_str())
            )),
            _ => None,
        })
        .collect()
}

fn start(fact: &Fact) -> u32 {
    fact.site().bytes.start
}

#[test]
fn es_imports_in_every_form() {
    let cases = [
        (
            "import React from 'react';",
            "Static react rel=0 names=[] star=- default=React in module",
        ),
        (
            "import { a, b as c } from \"./x\";",
            "Static ./x rel=1 names=[a, b as c] star=- default=- in module",
        ),
        (
            "import * as ns from '../y/z';",
            "Static ../y/z rel=2 names=[] star=ns default=- in module",
        ),
        (
            "import type { T } from './t';",
            "TypeOnly ./t rel=1 names=[T] star=- default=- in module",
        ),
        (
            "import './side';",
            "SideEffect ./side rel=1 names=[] star=- default=- in module",
        ),
        (
            "import D, { e } from 'pkg';",
            "Static pkg rel=0 names=[e] star=- default=D in module",
        ),
        (
            "import x = require('legacy');",
            "Require legacy rel=0 names=[] star=x default=- in module",
        ),
    ];
    for (source, expected) in cases {
        let facts = ts(source);
        assert_eq!(imports(&facts), vec![expected], "{source}");
        assert!(facts.losses.is_empty(), "{source}: {:?}", facts.losses);
        assert!(refs(&facts).is_empty(), "{source}: an import states no use");
    }
}

#[test]
fn commonjs_require_binds_what_the_call_is_assigned_to() {
    let cases = [
        (
            "const fs = require('fs');",
            "Require fs rel=0 names=[] star=fs default=- in module",
        ),
        (
            "const { a, b: c } = require('./m');",
            "Require ./m rel=1 names=[a, b as c] star=- default=- in module",
        ),
        (
            "const x = require('./m').x;",
            "Require ./m rel=1 names=[x] star=- default=- in module",
        ),
        (
            "const y = require('./m').other;",
            "Require ./m rel=1 names=[other as y] star=- default=- in module",
        ),
        (
            "require('./polyfill');",
            "Require ./polyfill rel=1 names=[] star=- default=- in module",
        ),
    ];
    for (source, expected) in cases {
        let facts = js(source);
        assert_eq!(imports(&facts), vec![expected], "{source}");
        assert!(facts.losses.is_empty(), "{source}: {:?}", facts.losses);
        assert!(
            ref_named(&facts, "require").is_empty(),
            "{source}: `require` is an import, not a call to resolve"
        );
    }
}

#[test]
fn dynamic_import_and_react_lazy() {
    let facts = ts(r#"const Card = React.lazy(() => import("./Card"));
export async function load() {
  const m = await import("./m");
  return m;
}
"#);
    assert_eq!(
        imports(&facts),
        vec![
            "Dynamic ./Card rel=1 names=[] star=- default=Card in callback React.lazy in def Card",
            "Dynamic ./m rel=1 names=[] star=m default=- in def load",
        ]
    );
    assert!(refs(&facts).contains(&"Call React.lazy in def Card".to_string()));
}

#[test]
fn a_dynamic_specifier_that_is_not_a_literal_states_no_import() {
    let facts = ts("export function pick(name: string) { return import(name); }");
    assert!(imports(&facts).is_empty());
}

#[test]
fn jsx_components_are_calls_and_intrinsic_tags_are_nothing() {
    let facts = ts(r#"import { Card } from "./Card";
export function Page({ items }: Props) {
  return (
    <div className="page">
      <Card title="x" onSelect={pick}>
        <Row.Cell value={items} />
        {items.map((i) => <Item key={i} />)}
      </Card>
    </div>
  );
}
"#);
    assert!(facts.losses.is_empty(), "{:?}", facts.losses);
    let seen = refs(&facts);
    for expected in [
        "TypeUse Props in def Page",
        "Call Card in def Page",
        "PassAsValue pick in def Page",
        "Call Row.Cell in def Page",
        "PassAsValue items in def Page",
        "Call items.map in def Page",
        "Call Item in callback items.map in def Page",
        "PassAsValue i in callback items.map in def Page",
    ] {
        assert!(
            seen.contains(&expected.to_string()),
            "{expected} not in {seen:#?}"
        );
    }
    assert!(
        seen.iter()
            .all(|r| !r.contains("div") && !r.contains("className")),
        "{seen:#?}"
    );
    assert_eq!(
        locals(&facts),
        vec![
            "Parameter items in def Page",
            "Parameter i in callback items.map in def Page"
        ]
    );
}

#[test]
fn template_literals_read_their_substitutions() {
    let facts = ts(
        r#"export function greet(user: User) { return `hi ${user.name} ${fmt(user.id)}`; }
export const tag = html`<b>${label}</b>`;
"#,
    );
    assert_eq!(
        refs(&facts),
        vec![
            "TypeUse User in def greet",
            "Read user.name in def greet",
            "Call fmt in def greet",
            "PassAsValue user.id in def greet",
            "Call html in def tag",
            "Read label in def tag",
        ]
    );
    let name = &ref_named(&facts, "name")[0];
    assert_eq!(receiver(&name.receiver), "typed User");
}

#[test]
fn a_class_property_arrow_is_a_method_and_its_locals_are_not_definitions() {
    let facts = ts(r#"export class Manager {
  private level = 0;

  private readonly passes = (): boolean => this.level < 4;

  submit = (correct: boolean, streak: number): number => {
    const promote = correct && streak >= 3 && this.passes();
    if (promote) {
      const promotedLevel = 4;
      this.level = promotedLevel;
    }
    return this.level;
  };
}
"#);
    assert_eq!(
        defs(&facts),
        vec![
            "Manager Class",
            "Manager.level Field",
            "Manager.passes Method",
            "Manager.submit Method",
        ]
    );
    assert_eq!(
        locals(&facts),
        vec![
            "Parameter correct in def Manager.submit",
            "Parameter streak in def Manager.submit",
            "Variable promote in def Manager.submit",
            "Variable promotedLevel in def Manager.submit",
        ]
    );
    assert_eq!(
        refs(&facts),
        vec![
            "Read level in def Manager.passes",
            "Read correct in def Manager.submit",
            "Read streak in def Manager.submit",
            "Call passes in def Manager.submit",
            "Read promote in def Manager.submit",
            "Write level in def Manager.submit",
            "Read promotedLevel in def Manager.submit",
            "Read level in def Manager.submit",
        ]
    );
    assert_eq!(receiver(&ref_named(&facts, "passes")[0].receiver), "self");
    assert!(matches!(
        def_named(&facts, "Manager.passes").visibility,
        Visibility::Private
    ));
    assert!(matches!(
        def_named(&facts, "Manager.submit").visibility,
        Visibility::Public
    ));
}

#[test]
fn a_multi_declarator_const_is_keyed_per_declarator() {
    let source = "const NAME = \"demo\", LEVEL = 3, MODEL = \"model-1\";\n\
                  export function describe() { return `${NAME}:${LEVEL}`; }\n\
                  export const hooks = { MODEL };\n";
    let facts = js(source);
    assert_eq!(
        defs(&facts),
        vec![
            "NAME Constant",
            "LEVEL Constant",
            "MODEL Constant",
            "describe Function",
            "hooks Constant",
        ]
    );
    for (name, written) in [
        ("NAME", "NAME = \"demo\""),
        ("LEVEL", "LEVEL = 3"),
        ("MODEL", "MODEL = \"model-1\""),
    ] {
        let span = &def_named(&facts, name).span;
        assert_eq!(
            &source[span.bytes.start as usize..span.bytes.end as usize],
            written
        );
    }
    assert_eq!(
        refs(&facts),
        vec![
            "Read NAME in def describe",
            "Read LEVEL in def describe",
            "Read MODEL in def hooks",
        ]
    );
}

#[test]
fn re_exports_keep_their_source_and_shape() {
    let facts = ts(r#"export { a, b as c } from "./m";
export * from "./all";
export * as ns from "./ns";
export type { T } from "./types";
const local = 1;
export { local };
"#);
    assert_eq!(
        exports(&facts),
        vec![
            "Named a from ./m",
            "Named b as c from ./m",
            "Star ./all",
            "Star ./ns as ns",
            "Named T from ./types type",
            "Named local",
        ]
    );
    assert!(matches!(
        def_named(&facts, "local").visibility,
        Visibility::Public
    ));
}

#[test]
fn an_anonymous_default_export_is_a_definition_named_default() {
    let cases: [(&str, &[&str], &[&str], &[&str]); 4] = [
        (
            "export default function () { return helper(); }",
            &["default Function"],
            &["Default -"],
            &["Call helper in def default"],
        ),
        (
            "export default class { run() { return 1; } }",
            &["default Class", "default.run Method"],
            &["Default -"],
            &[],
        ),
        (
            "export default foo;",
            &[],
            &["Default foo"],
            &["Read foo in module"],
        ),
        (
            "export default () => run();",
            &["default Function"],
            &["Default -"],
            &["Call run in def default"],
        ),
    ];
    for (source, want_defs, want_exports, want_refs) in cases {
        let facts = ts(source);
        assert_eq!(defs(&facts), want_defs, "{source}");
        assert_eq!(exports(&facts), want_exports, "{source}");
        assert_eq!(refs(&facts), want_refs, "{source}");
    }
}

#[test]
fn overloads_and_declaration_merging_are_told_apart_by_ordinal_never_by_signature() {
    let facts = ts(r#"export function parse(input: string): string;
export function parse(input: number): number;
export function parse(input: any) { return input; }
interface Box { a: number }
interface Box { b: number }
class Box {}
"#);
    assert_eq!(
        defs(&facts),
        vec![
            "parse#0 Function",
            "parse#1 Function",
            "parse#2 Function",
            "Box#0@type Interface",
            "Box.a Property",
            "Box#1@type Interface",
            "Box.b Property",
            "Box Class",
        ]
    );
    let signatures: Vec<String> = definitions(&facts)
        .iter()
        .filter(|def| def.local.name.as_str() == "parse")
        .map(|def| def.signature.clone().expect("a signature"))
        .collect();
    assert!(signatures[0].contains("string") && signatures[1].contains("number"));
    assert_ne!(signatures[0], signatures[1]);
}

#[test]
fn enums_namespaces_and_type_aliases_are_definitions_with_their_members() {
    let facts = ts(r#"export enum Color { Red, Green = 2 }
export type Id = string | number;
namespace Util {
  export const VERSION = 1;
  export function help() {}
}
"#);
    assert_eq!(
        defs(&facts),
        vec![
            "Color Enum",
            "Color.Red EnumMember",
            "Color.Green EnumMember",
            "Id@type TypeAlias",
            "Util Module",
            "Util.VERSION Constant",
            "Util.help Function",
        ]
    );
    assert!(matches!(
        def_named(&facts, "Util.VERSION").visibility,
        Visibility::Public
    ));
}

#[test]
fn locals_and_parameters_are_bindings_never_definitions() {
    let facts = ts(
        r#"export function outer(a: number, { b }: Opts, ...rest: number[]) {
  const inner = (x: number) => x + a;
  function helper(y: number) { return y; }
  for (const item of rest) { use(item); }
  try { run(); } catch (err) { log(err); }
  return inner(b);
}
"#,
    );
    assert_eq!(defs(&facts), vec!["outer Function"]);
    assert_eq!(
        locals(&facts),
        vec![
            "Parameter a in def outer",
            "Parameter b in def outer",
            "Parameter rest in def outer",
            "Closure inner in def outer",
            "Parameter x in def outer",
            "Closure helper in def outer",
            "Parameter y in def outer",
            "Variable item in def outer",
            "Variable err in def outer",
        ]
    );
}

#[test]
fn a_nested_function_or_class_is_a_definition_only_when_it_leaves_its_function() {
    let facts = ts(r#"export function outer() {
  function inner(x: number) { return helper(x); }
  const run = () => {
    function deep() { return inner(1); }
    return deep();
  };
  return inner(1) + run();
}
export function factory(target: Target, items: number[]) {
  function onTick() { return 1; }
  function made() { return 2; }
  function listed() { return 3; }
  function kept() { return 4; }
  function loud() { return 5; }
  function each(n: number) { return n; }
  function suite() { return 6; }
  class Impl { run() { return 7; } }
  class Hidden { run() { return 8; } }
  target.onTick = onTick;
  const bag = { kept };
  const hidden = new Hidden();
  setTimeout(loud);
  items.map(each);
  it("works", suite);
  return { made, handlers: [listed], bag, impl: new Impl() };
}
export const useStore = create()((set) => {
  function alpha() { return 1; }
  function beta() { return alpha(); }
  return { play: async () => { await beta(); }, alpha };
});
"#);
    assert!(facts.losses.is_empty(), "{:?}", facts.losses);
    let mut named = defs(&facts);
    named.sort();
    let mut expected = vec![
        "outer Function",
        "factory Function",
        "factory.onTick Function",
        "factory.made Function",
        "factory.listed Function",
        "factory.kept Function",
        "factory.loud Function",
        "factory.Impl Class",
        "factory.Impl.run Method",
        "useStore Constant",
        "useStore.alpha Function",
        "useStore.play Method",
    ];
    expected.sort();
    assert_eq!(named, expected);
    let seen = refs(&facts);
    for wanted in [
        "Call helper in def outer",
        "Call inner in def outer",
        "Call deep in def outer",
        "Call alpha in callback create in def useStore",
    ] {
        assert!(
            seen.contains(&wanted.to_string()),
            "{wanted} not in {seen:#?}"
        );
    }
    let bound = locals(&facts);
    for wanted in [
        "Closure inner in def outer",
        "Closure deep in def outer",
        "Closure each in def factory",
        "Closure suite in def factory",
        "Closure Hidden in def factory",
        "Closure beta in callback create in def useStore",
    ] {
        assert!(
            bound.contains(&wanted.to_string()),
            "{wanted} not in {bound:#?}"
        );
    }
    assert!(
        bound.iter().all(|l| !l.contains("Closure alpha")
            && !l.contains("Closure made")
            && !l.contains("Closure Impl")),
        "{bound:?}"
    );
}

#[test]
fn a_parameter_named_like_a_nested_function_is_not_that_function() {
    let facts = ts(r#"export function make() {
  function alpha() { return 1; }
  function delta(alpha: number) { return alpha; }
  return { delta };
}
"#);
    let mut named = defs(&facts);
    named.sort();
    assert_eq!(named, vec!["make Function", "make.delta Function"]);
}

#[test]
fn the_methods_of_an_object_literal_a_function_returns_are_its_definitions() {
    let facts = ts(r#"export function makeSys() {
  return {
    async open(): Promise<string> { return helper(); },
    close: () => 1,
    flag: true,
  };
}
export const make = () => ({ run() { return 1; } });
export function configure() {
  return use({ onDone() {}, queryFn: () => fetch("/x") });
}
"#);
    assert_eq!(
        defs(&facts),
        vec![
            "makeSys Function",
            "makeSys.open Method",
            "makeSys.close Method",
            "make Function",
            "make.run Method",
            "configure Function",
        ]
    );
    assert!(refs(&facts).contains(&"Call helper in def makeSys.open".to_string()));
    assert!(
        refs(&facts).contains(&"Call fetch in callback use in def configure".to_string()),
        "an object literal passed to a call is configuration, not definitions (#706): {:?}",
        refs(&facts)
    );
}

#[test]
fn callbacks_passed_to_calls_carry_the_callee() {
    let facts = ts(r#"import { isTasteLine } from "./taste";
test("promotes", () => { expect(run(1)).toBe(2); });
export function writeTaste(text: string) {
  return text.split("\n").filter((line) => !isTasteLine(line));
}
"#);
    assert_eq!(
        refs(&facts),
        vec![
            "Call test in module",
            "Call expect in callback test in module",
            "Call run in callback test in module",
            "Call toBe in callback test in module",
            "Call text.split in def writeTaste",
            "Call filter in def writeTaste",
            "Call isTasteLine in callback text.split.filter in def writeTaste",
            "PassAsValue line in callback text.split.filter in def writeTaste",
        ]
    );
}

#[test]
fn nested_callbacks_nest_their_scopes() {
    let facts = ts(r#"describe("suite", () => {
  it("case", () => { check(); });
});
"#);
    assert_eq!(
        refs(&facts),
        vec![
            "Call describe in module",
            "Call it in callback describe in module",
            "Call check in callback it in callback describe in module",
        ]
    );
}

#[test]
fn an_object_literal_property_passed_to_a_call_is_passed_as_a_value() {
    let facts = ts(r#"import { useQuery } from "./lib";
export function useThing(onDone: () => void) {
  const mutationFn = save;
  return useQuery({ queryKey: key, queryFn: () => fetch("/thing"), mutationFn, onSuccess: onDone });
}
"#);
    assert_eq!(defs(&facts), vec!["useThing Function"]);
    assert_eq!(
        refs(&facts),
        vec![
            "Read save in def useThing",
            "Call useQuery in def useThing",
            "PassAsValue key in def useThing",
            "Call fetch in callback useQuery in def useThing",
            "PassAsValue mutationFn in def useThing",
            "PassAsValue onDone in def useThing",
        ]
    );
}

#[test]
fn a_receiver_is_typed_by_new_by_an_annotation_or_by_a_promoted_parameter() {
    let facts = ts(r#"import { ItemsService } from "./items";
const items = new ItemsService("/x/");
export async function load(ids: string[]) { return items.byIds(ids); }
export async function loadFresh(ids: string[]) { return new ItemsService("/y/").byIds(ids); }
export async function loadTyped(svc: ItemsService, ids: string[]) { return svc.byIds(ids); }
class Page {
  constructor(private readonly api: Api) {}
  run() { return this.api.get(); }
}
"#);
    let seen: Vec<String> = ref_named(&facts, "byIds")
        .iter()
        .map(|r| format!("{}: {}", scope(&r.enclosing), receiver(&r.receiver)))
        .collect();
    assert_eq!(
        seen,
        vec![
            "def load: typed ItemsService",
            "def loadFresh: new ItemsService",
            "def loadTyped: typed ItemsService",
        ]
    );
    let instantiated: Vec<String> = refs(&facts)
        .into_iter()
        .filter(|r| r.starts_with("Instantiate"))
        .collect();
    assert_eq!(
        instantiated,
        vec![
            "Instantiate ItemsService in def items",
            "Instantiate ItemsService in def loadFresh",
        ]
    );
    let get = &ref_named(&facts, "get")[0];
    assert_eq!(qualified(get), "this.api.get");
    assert_eq!(receiver(&get.receiver), "typed Api");
    assert_eq!(
        defs(&facts)[4..],
        [
            "Page Class",
            "Page.constructor Constructor",
            "Page.api Field",
            "Page.run Method",
        ]
    );
}

#[test]
fn decorators_docs_and_visibility_are_stated_on_the_definition() {
    let facts = ts(r#"/** Adds. */
@Injectable()
export class Svc {
  @Input() name = "";
  private secret = 1;
  #hidden = 2;
}
"#);
    let svc = def_named(&facts, "Svc");
    assert_eq!(
        svc.decorators
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>(),
        ["Injectable"]
    );
    assert!(svc.doc_hash.is_some());
    assert!(matches!(svc.visibility, Visibility::Public));
    assert_eq!(
        def_named(&facts, "Svc.name")
            .decorators
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>(),
        ["Input"]
    );
    assert!(matches!(
        def_named(&facts, "Svc.secret").visibility,
        Visibility::Private
    ));
    assert!(matches!(
        def_named(&facts, "Svc.#hidden").visibility,
        Visibility::Private
    ));
    assert!(
        refs(&facts).contains(&"Decorate Injectable in def Svc".to_string()),
        "{:?}",
        refs(&facts)
    );
}

#[test]
fn a_doc_comment_is_hashed_by_its_words_not_its_layout() {
    let one = ts("/** Adds   two. */\nexport function add() {}\n");
    let other = ts("/**\n * Adds two.\n */\nexport function add() {}\n");
    let changed = ts("/** Adds three. */\nexport function add() {}\n");
    let plain = ts("export function add() {}\n");
    assert!(def_named(&one, "add").doc_hash.is_some());
    assert_eq!(
        def_named(&one, "add").doc_hash,
        def_named(&other, "add").doc_hash
    );
    assert_ne!(
        def_named(&one, "add").doc_hash,
        def_named(&changed, "add").doc_hash
    );
    assert_eq!(def_named(&plain, "add").doc_hash, None);
}

#[test]
fn the_hashes_tell_layout_from_a_body_change() {
    let base = ts("export function f(a: number) { return a + 1; }");
    let relaid = ts("export function f(a: number) {\n  return a  +  1;\n}");
    let edited = ts("export function f(a: number) { return a + 2; }");
    let resigned = ts("export function f(a: string) { return a + 1; }");
    let (base, relaid, edited, resigned) = (
        def_named(&base, "f"),
        def_named(&relaid, "f"),
        def_named(&edited, "f"),
        def_named(&resigned, "f"),
    );
    assert_eq!(base.body_hash, relaid.body_hash);
    assert_ne!(base.text_hash, relaid.text_hash);
    assert_eq!(base.signature, relaid.signature);
    assert_ne!(base.body_hash, edited.body_hash);
    assert_ne!(base.signature, resigned.signature);
    assert_eq!(base.body_hash, resigned.body_hash);
    assert_ne!(base.body_hash, 0);

    let asi = ts("function g() { return\n 1; }");
    let joined = ts("function g() { return 1; }");
    assert_ne!(
        def_named(&asi, "g").body_hash,
        def_named(&joined, "g").body_hash,
        "a line break that changes ASI is a change of the body"
    );
}

#[test]
fn a_member_change_is_the_members_change_alone() {
    let before = ts("class A { x() { return 1; } y() { return 2; } }");
    let after = ts("class A { x() { return 9; } y() { return 2; } }");
    assert_eq!(
        def_named(&before, "A").body_hash,
        def_named(&after, "A").body_hash
    );
    assert_eq!(
        def_named(&before, "A").text_hash,
        def_named(&after, "A").text_hash
    );
    assert_ne!(
        def_named(&before, "A.x").body_hash,
        def_named(&after, "A.x").body_hash
    );
    assert_eq!(
        def_named(&before, "A.y").body_hash,
        def_named(&after, "A.y").body_hash
    );
}

#[test]
fn a_parse_error_is_a_loss_beside_what_was_read() {
    let facts = ts(
        "export function good() { return 1; }\nconst = ;\nexport function after() { return good(); }\n",
    );
    assert!(
        defs(&facts).contains(&"good Function".to_string()),
        "{:?}",
        defs(&facts)
    );
    let lines: Vec<&Vec<(u32, u32)>> = facts
        .losses
        .iter()
        .filter_map(|loss| match loss {
            Loss::ParseError { lines, .. } => Some(lines),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1, "{:?}", facts.losses);
    assert!(
        lines[0].iter().any(|(from, to)| *from <= 2 && 2 <= *to),
        "{lines:?}"
    );
}

#[test]
fn a_clean_parse_has_no_loss() {
    assert!(ts("export const a = 1;\n").losses.is_empty());
    assert!(js("export const a = 1;\n").losses.is_empty());
}

#[test]
fn a_spent_budget_ends_the_walk_with_a_deadline_loss() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let facts = TypeScript.extract(b"export const a = 1;\n", &budget);
    assert!(
        facts
            .losses
            .iter()
            .any(|loss| matches!(loss, Loss::Deadline { .. })),
        "{:?}",
        facts.losses
    );
    assert!(facts.facts.is_empty());
}

#[test]
fn facts_come_in_source_order_and_the_same_bytes_give_the_same_facts() {
    let source = "import a from './a';\nexport function f() { return a(g()); }\nfunction g() { return 1; }\nexport { g };\n";
    let first = ts(source);
    let second = ts(source);
    assert_eq!(format!("{:?}", first.facts), format!("{:?}", second.facts));
    let starts: Vec<u32> = first.facts.iter().map(start).collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] <= pair[1]),
        "{starts:?}"
    );
    assert!(starts.len() >= 6);
}

#[test]
fn javascript_classes_and_calls() {
    let facts = js(r#"import fs from 'fs';
export class Repo extends Base {
  load() { return fs.readFileSync(this.path); }
  static create() { return new Repo(); }
}
"#);
    assert_eq!(
        defs(&facts),
        vec!["Repo Class", "Repo.load Method", "Repo.create Method"]
    );
    let seen = refs(&facts);
    for expected in [
        "TypeUse Base in def Repo",
        "Call fs.readFileSync in def Repo.load",
        "PassAsValue path in def Repo.load",
        "Instantiate Repo in def Repo.create",
    ] {
        assert!(
            seen.contains(&expected.to_string()),
            "{expected} not in {seen:#?}"
        );
    }
}

#[test]
fn commonjs_exports() {
    let facts = js(r#"function a() {}
function b() {}
module.exports = { a, renamed: b };
exports.c = function () { return a(); };
"#);
    assert_eq!(
        exports(&facts),
        vec!["Named a", "Named b as renamed", "Named c"]
    );
    assert!(
        defs(&facts).contains(&"c Function".to_string()),
        "{:?}",
        defs(&facts)
    );
    assert!(
        refs(&facts).contains(&"Call a in def c".to_string()),
        "{:?}",
        refs(&facts)
    );
}

#[test]
fn the_file_serves_both_languages() {
    let languages: Vec<String> = _diffctx::facts::lang::typescript::extractors()
        .iter()
        .map(|extractor| extractor.language().to_string())
        .collect();
    assert_eq!(languages, ["typescript", "javascript"]);
}

#[test]
fn a_change_inside_a_member_changes_the_member_alone() {
    let hashes = |source: &str, name: &str| {
        let facts = ts(source);
        let def = def_named(&facts, name);
        (def.body_hash, def.text_hash)
    };
    // An object literal a function returns: the function does not change.
    let make = |body: &str| {
        format!(
            "export function makeSys() {{\n  return {{\n    open() {{ return {body}; }},\n    flag: true,\n  }};\n}}\n"
        )
    };
    let (before, after) = (make("'a'"), make("'b'"));
    assert_eq!(hashes(&before, "makeSys"), hashes(&after, "makeSys"));
    assert_ne!(
        hashes(&before, "makeSys.open").0,
        hashes(&after, "makeSys.open").0
    );
    // A function declared inside another and returned by it.
    let outer = |body: &str| {
        format!(
            "export function outer() {{\n  function inner() {{ return {body}; }}\n  return inner;\n}}\n"
        )
    };
    assert_eq!(hashes(&outer("1"), "outer"), hashes(&outer("2"), "outer"));
    assert_ne!(
        hashes(&outer("1"), "outer.inner").0,
        hashes(&outer("2"), "outer.inner").0
    );
    // One that never leaves is part of the function: its change is the
    // function's.
    let local = |body: &str| {
        format!(
            "export function outer() {{\n  function inner() {{ return {body}; }}\n  return inner();\n}}\n"
        )
    };
    assert_ne!(
        hashes(&local("1"), "outer").0,
        hashes(&local("2"), "outer").0
    );
    assert_eq!(defs(&ts(&local("1"))), vec!["outer Function"]);
    // A class gains a property arrow: the class, whose members are definitions
    // of their own, keeps its hashes, separators included.
    let manager = |extra: &str| {
        format!(
            "export class Manager {{\n  private level = 0;\n{extra}  submit = (n: number): number => {{\n    return n + this.level;\n  }};\n}}\n"
        )
    };
    assert_eq!(
        hashes(&manager(""), "Manager"),
        hashes(
            &manager("  private readonly passes = (): boolean => this.level < 4;\n"),
            "Manager"
        )
    );
    // The container's own signature is still its change.
    let signed = |params: &str| {
        format!("export function f({params}) {{ return {{ g() {{ return 1; }} }}; }}\n")
    };
    assert_ne!(hashes(&signed("a: number"), "f").0, 0);
    assert_ne!(
        ts(&signed("a: number")).facts.iter().find_map(|f| match f {
            Fact::Definition(d) if d.local.name.as_str() == "f" => d.signature.clone(),
            _ => None,
        }),
        ts(&signed("a: string")).facts.iter().find_map(|f| match f {
            Fact::Definition(d) if d.local.name.as_str() == "f" => d.signature.clone(),
            _ => None,
        })
    );
}

#[test]
fn the_members_of_an_object_type_and_of_a_callbacks_object_are_definitions() {
    let facts = ts(
        r#"export type Prefs = { softPrefs: string[]; save(): void };
export const usePlayer = create()((set) => ({
  updateLyrics: (lyrics: string) => { set({ lyrics }); },
  count: 0,
}));
"#,
    );
    assert_eq!(
        defs(&facts),
        vec![
            "Prefs@type TypeAlias",
            "Prefs.softPrefs Property",
            "Prefs.save Method",
            "usePlayer Constant",
            "usePlayer.updateLyrics Method",
        ]
    );
}

#[test]
fn a_global_object_assignment_is_a_definition_and_a_script_load_is_an_import() {
    let facts = js(r#"importScripts('/kit/sw-kit.js', "../x.js");
self.serveShell = ({ name }) => name;
globalThis.version = 1;
function local() { self.inside = 2; }
"#);
    assert_eq!(
        defs(&facts),
        vec!["serveShell Function", "version Variable", "local Function"]
    );
    assert_eq!(
        imports(&facts),
        vec![
            "SideEffect /kit/sw-kit.js rel=0 names=[] star=- default=- in module",
            "SideEffect ../x.js rel=2 names=[] star=- default=- in module",
        ]
    );
    assert!(ref_named(&facts, "serveShell").is_empty());
    assert_eq!(ref_named(&facts, "inside").len(), 1);
}

#[test]
fn a_glob_and_a_require_context_state_the_paths_they_load() {
    let facts = ts(
        r#"const a = import.meta.glob(['../lib/**/*.ts', '!../lib/skip/**']);
const b = import.meta.glob('./pages/*.ts', { eager: true });
const c = require.context('../lib/live', false, /\.ts$/);
const d = require.context('./all', true);
"#,
    );
    let mentions: Vec<String> = facts
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::PathMention(m) => Some(m.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        mentions,
        vec![
            "../lib/**/*.ts",
            "!../lib/skip/**",
            "./pages/*.ts",
            "../lib/live/*",
            "./all/**/*"
        ]
    );
}

#[test]
fn a_bare_ampersand_in_jsx_text_is_text() {
    let facts = ts(
        "import { apps } from './api';\nexport function Panel() {\n  return (\n    <h2>Devices & apps &amp; more</h2>\n  );\n}\n",
    );
    assert!(facts.losses.is_empty(), "{:?}", facts.losses);
    assert!(ref_named(&facts, "apps").is_empty(), "{:?}", refs(&facts));
}

#[test]
fn the_methods_of_an_object_literal_a_constant_holds_are_its_definitions() {
    let facts = ts(r#"export const cart = {
  total() { return 1; },
  tax: (x: number) => x,
  label: "cart",
};
"#);
    assert_eq!(
        defs(&facts),
        vec!["cart Constant", "cart.total Method", "cart.tax Method"]
    );
}

#[test]
fn the_members_of_an_interface_or_object_type_are_part_of_the_type() {
    let body = |source: &str, name: &str| def_named(&ts(source), name).body_hash;
    let iface = |ty: &str| format!("export interface Opts {{\n  a: {ty};\n  b: number;\n}}\n");
    assert_ne!(
        body(&iface("string"), "Opts@type"),
        body(&iface("number"), "Opts@type"),
        "a member's type is the interface's"
    );
    let facts = ts(&iface("string"));
    let member = def_named(&facts, "Opts.a");
    assert_eq!(member.body_hash, 0, "a member has no body to pair by");
    assert_ne!(member.text_hash, 0, "and is a definition of its own");
    let alias = |ty: &str| format!("export type Opts = {{ a: {ty}; b: number }};\n");
    assert_ne!(
        body(&alias("string"), "Opts@type"),
        body(&alias("number"), "Opts@type")
    );
}

#[test]
fn a_function_assigned_to_a_member_defines_a_method_and_holds_its_own_functions() {
    let facts = js(r#"var proto = module.exports = function () {};
proto.handle = function handle(req) {
  function next(err) { return trim(err); }
  function trim(x) { return x; }
  return next;
};
Foo.prototype.bar = function () { return 1; };
res.send = (x) => x;
exports.keep = function () {};
"#);
    let named = defs(&facts);
    for expected in [
        "proto.handle Method",
        "proto.handle.next Function",
        "Foo.bar Method",
        "res.send Method",
    ] {
        assert!(
            named.contains(&expected.to_string()),
            "{expected} not in {named:?}"
        );
    }
    assert_eq!(
        ref_named(&facts, "handle").len(),
        1,
        "the write stays: it reads `proto`"
    );
    assert!(
        !named.contains(&"proto.handle.trim Function".to_string()),
        "a function the method only calls is local: {named:?}"
    );
    assert!(refs(&facts).contains(&"Call trim in def proto.handle.next".to_string()));
    assert!(locals(&facts).contains(&"Closure trim in def proto.handle".to_string()));
}
