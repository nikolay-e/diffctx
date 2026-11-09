//! The substrate of wave 7's config family: YAML, JSON, TOML and INI read into one
//! tree with spans, the objects and references read off it, the pointers
//! resolved as text. Fixtures are small enough to read the right answer off;
//! nothing here runs diffctx to learn one.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::config::{
    Dialect, Flaw, Node, Options, PointerKind, Reading, Resolved, Row, ScalarKind, Segment,
    SourcePointer, Strength, Templating, Value, directory_of, extract, objects, read,
};
use _diffctx::facts::{Scope, Span, config::FieldPath};
use _diffctx::snapshot::RunBudget;

fn load(dialect: Dialect, text: &str) -> Reading {
    read(
        dialect,
        text.as_bytes(),
        Options::default(),
        &RunBudget::default(),
    )
}

fn yaml(text: &str) -> Reading {
    load(Dialect::Yaml, text)
}

fn helm(text: &str) -> Reading {
    read(
        Dialect::Yaml,
        text.as_bytes(),
        Options {
            templating: Templating::Go,
        },
        &RunBudget::default(),
    )
}

fn root(reading: &Reading) -> &Node {
    reading.documents.first().expect("a document")
}

fn text_of<'a>(source: &'a str, span: &Span) -> &'a str {
    &source[span.bytes.start as usize..span.bytes.end as usize]
}

fn str_at<'a>(node: &'a Node, path: &str) -> Option<&'a str> {
    node.at(path).and_then(Node::as_str)
}

// --- YAML ---

const MANIFESTS: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  namespace: shop
spec:
  replicas: 3
---
apiVersion: v1
kind: Service
metadata:
  name: api
---
kind: ConfigMap
data:
  a: b
";

#[test]
fn a_yaml_stream_is_one_document_per_separator_and_its_objects_have_identities() {
    let reading = yaml(MANIFESTS);
    assert!(reading.is_whole(), "{reading:?}");
    assert_eq!(reading.documents.len(), 3);
    let found = objects(&reading);
    let ids: Vec<String> = found
        .iter()
        .map(|f| {
            let id = &f.object.id;
            format!(
                "{}|{}|{}|{}",
                id.group,
                id.kind,
                id.namespace.as_deref().unwrap_or("-"),
                id.name
            )
        })
        .collect();
    // The Service and the Deployment share a name and are two objects; the
    // ConfigMap has no apiVersion and states none.
    assert_eq!(ids, ["apps|Deployment|shop|api", "|Service|-|api"]);
    assert_eq!(found[0].object.version, "v1");
    for f in &found {
        assert_eq!(text_of(MANIFESTS, &f.object.name_site), "api");
        assert!(!f.object.templated);
    }
    assert_eq!(found[0].object.name_site.lines, (4, 4));
    assert_eq!(found[1].object.name_site.lines, (12, 12));
}

#[test]
fn a_list_states_the_objects_of_its_items_and_none_of_its_own() {
    let reading = yaml(
        "apiVersion: v1
kind: List
items:
  - apiVersion: v1
    kind: ConfigMap
    metadata:
      name: one
  - apiVersion: v1
    kind: Secret
    metadata:
      name: two
      namespace: ns
",
    );
    let kinds: Vec<_> = objects(&reading)
        .iter()
        .map(|f| (f.object.id.kind.clone(), f.object.id.name.clone()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("ConfigMap".to_string(), "one".to_string()),
            ("Secret".to_string(), "two".to_string())
        ]
    );
}

#[test]
fn yaml_scalars_are_decoded_and_typed() {
    let source = r#"plain: hello world
int: 42
float: 1.5
bool: true
no: ~
empty:
note: v # trailing
quoted: "a\tbé"
single: 'it''s'
str_tag: !!str 80
quoted_int: "80"
folded_plain: one
  two

  three
lit: |
  line one
  line two
strip: |-
  x
fold: >
  a
  b

  c
ind: |2
    two extra
"#;
    let reading = yaml(source);
    assert!(reading.is_whole(), "{reading:?}");
    let doc = root(&reading);
    assert_eq!(str_at(doc, "plain"), Some("hello world"));
    let int = doc.get("int").and_then(Node::as_scalar).expect("a scalar");
    assert_eq!((int.text.as_str(), int.kind), ("42", ScalarKind::Int));
    assert_eq!(doc.get("int").and_then(Node::as_str), None);
    assert_eq!(
        doc.get("float").and_then(Node::as_scalar).map(|s| s.kind),
        Some(ScalarKind::Float)
    );
    assert_eq!(doc.get("bool").and_then(Node::as_bool), Some(true));
    assert!(doc.get("no").is_some_and(Node::is_null));
    assert!(doc.get("empty").is_some_and(Node::is_null));
    assert_eq!(str_at(doc, "note"), Some("v"));
    assert_eq!(str_at(doc, "quoted"), Some("a\tb\u{e9}"));
    assert_eq!(str_at(doc, "single"), Some("it's"));
    assert_eq!(str_at(doc, "str_tag"), Some("80"));
    assert_eq!(str_at(doc, "quoted_int"), Some("80"));
    assert!(
        doc.get("quoted_int")
            .and_then(Node::as_scalar)
            .is_some_and(|s| s.quoted)
    );
    assert_eq!(str_at(doc, "folded_plain"), Some("one two\nthree"));
    assert_eq!(str_at(doc, "lit"), Some("line one\nline two\n"));
    assert_eq!(str_at(doc, "strip"), Some("x"));
    assert_eq!(str_at(doc, "fold"), Some("a b\nc\n"));
    assert_eq!(str_at(doc, "ind"), Some("  two extra\n"));
}

#[test]
fn a_node_knows_the_text_it_was_read_from() {
    let reading = yaml(MANIFESTS);
    let doc = root(&reading);
    let name = doc.at("metadata.name").expect("a name");
    assert_eq!(text_of(MANIFESTS, &name.span), "api");
    assert_eq!(name.span.lines, (4, 4));
    let spec = doc.get("spec").expect("a spec");
    assert_eq!(spec.span.lines, (7, 7));
    let key = &doc.entries()[2].key;
    assert_eq!((key.text.as_str(), key.span.lines), ("metadata", (3, 3)));
    assert_eq!(text_of(MANIFESTS, &key.span), "metadata");
}

#[test]
fn flow_collections_sequences_and_a_key_without_a_value() {
    let reading = yaml(
        "flow: {a: 1, b: [x, \"y\"], c}
seq:
  - a
  - b: 1
    c: 2
",
    );
    assert!(reading.is_whole(), "{reading:?}");
    let doc = root(&reading);
    assert_eq!(
        doc.at("flow.a").and_then(Node::as_scalar).map(|s| s.kind),
        Some(ScalarKind::Int)
    );
    let b = doc.at("flow.b").and_then(Node::as_seq).expect("a sequence");
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].as_str(), Some("y"));
    assert!(doc.at("flow.c").is_some_and(Node::is_null));
    let seq = doc.get("seq").and_then(Node::as_seq).expect("a sequence");
    assert_eq!(seq[0].as_str(), Some("a"));
    assert_eq!(
        seq[1]
            .at("c")
            .and_then(Node::as_scalar)
            .map(|s| s.text.as_str()),
        Some("2")
    );
}

#[test]
fn an_alias_is_the_anchored_node_and_a_merge_key_is_looked_through() {
    let reading = yaml(
        "base: &base
  image: nginx
  port: 80
svc:
  <<: *base
  port: 8080
list:
  - &item one
  - *item
",
    );
    assert!(reading.is_whole(), "{reading:?}");
    let doc = root(&reading);
    assert_eq!(str_at(doc, "svc.image"), Some("nginx"));
    // An explicit key wins over a merged one.
    assert_eq!(
        doc.at("svc.port")
            .and_then(Node::as_scalar)
            .map(|s| s.text.as_str()),
        Some("8080")
    );
    let list = doc.get("list").and_then(Node::as_seq).expect("a sequence");
    assert_eq!(
        list.iter().filter_map(Node::as_str).collect::<Vec<_>>(),
        ["one", "one"]
    );
    // The copy keeps the spans of the text it was copied from.
    assert_eq!(list[0].span, list[1].span);
}

#[test]
fn an_alias_to_nothing_is_named_and_the_read_is_not_whole() {
    let reading = yaml("x: *missing\ny: 1\n");
    assert!(
        reading.flaws.contains(&Flaw::UnresolvedAlias {
            name: "missing".to_string(),
            line: 1
        }),
        "{:?}",
        reading.flaws
    );
    assert!(!reading.is_whole());
    assert_eq!(reading.errors, [(1, 1)]);
    assert!(root(&reading).get("x").is_some_and(Node::is_null));
}

#[test]
fn the_last_of_two_equal_keys_wins_and_both_are_listed() {
    let reading = yaml("a: 1\na: 2\n");
    let doc = root(&reading);
    assert_eq!(
        doc.get("a")
            .and_then(Node::as_scalar)
            .map(|s| s.text.as_str()),
        Some("2")
    );
    assert_eq!(doc.get_all("a").count(), 2);
}

#[test]
fn a_parse_error_is_a_line_range_and_a_whole_file_has_none() {
    assert!(yaml("good: 1\n").is_whole());
    let broken = yaml("good: 1\nbad: [unclosed\nother: 2\n");
    assert!(!broken.errors.is_empty(), "{broken:?}");
    assert!(!broken.is_whole());
}

#[test]
fn a_file_that_is_not_utf8_is_named_and_nothing_is_read() {
    let reading = read(
        Dialect::Yaml,
        &[0xff, 0xfe, b'a'],
        Options::default(),
        &RunBudget::default(),
    );
    assert_eq!(reading.flaws, [Flaw::NotUtf8]);
    assert!(reading.documents.is_empty());
}

// --- Go templates ---

const CHART_TEMPLATE: &str = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ include "app.fullname" . }}
  labels:
    {{- include "app.labels" . | nindent 4 }}
spec:
  {{- if .Values.autoscaling.enabled }}
  replicas: {{ .Values.replicaCount }}
  {{- end }}
  template:
    spec:
      containers:
        - name: app
          image: "{{ .Values.image.repository }}:{{ .Values.image.tag }}"
"#;

#[test]
fn a_go_template_is_read_around_and_its_actions_are_kept() {
    let reading = helm(CHART_TEMPLATE);
    assert!(reading.is_whole(), "{reading:?}");
    let actions: Vec<(&str, bool)> = reading
        .templates
        .iter()
        .map(|t| (t.text.as_str(), t.standalone))
        .collect();
    assert_eq!(
        actions,
        [
            (r#"{{ include "app.fullname" . }}"#, false),
            (r#"{{- include "app.labels" . | nindent 4 }}"#, true),
            ("{{- if .Values.autoscaling.enabled }}", true),
            ("{{ .Values.replicaCount }}", false),
            ("{{- end }}", true),
            ("{{ .Values.image.repository }}", false),
            ("{{ .Values.image.tag }}", false),
        ]
    );
    for t in &reading.templates {
        assert_eq!(text_of(CHART_TEMPLATE, &t.span), t.text);
    }
    let doc = root(&reading);
    assert!(doc.at("metadata.name").is_some_and(|n| n.templated));
    assert!(doc.at("spec.replicas").is_some_and(|n| n.templated));
    let container = doc
        .at("spec.template.spec.containers")
        .and_then(Node::as_seq)
        .and_then(<[Node]>::first)
        .expect("a container");
    assert_eq!(container.get("name").and_then(Node::as_str), Some("app"));
    assert!(!container.get("name").expect("a name").templated);
    assert!(container.get("image").is_some_and(|n| n.templated));
    // The identity of the object is written by a template: it names no object.
    let found = objects(&reading);
    assert_eq!(found.len(), 1);
    assert!(found[0].object.templated);
}

#[test]
fn an_action_holding_closing_braces_in_a_string_or_a_comment_is_one_action() {
    let reading = helm("a: {{ printf \"}}\" }}\nb: 1\n{{/* note }} */}}\nc: 2\n");
    assert!(reading.is_whole(), "{reading:?}");
    let actions: Vec<(&str, bool)> = reading
        .templates
        .iter()
        .map(|t| (t.text.as_str(), t.standalone))
        .collect();
    assert_eq!(
        actions,
        [("{{ printf \"}}\" }}", false), ("{{/* note }} */}}", true)]
    );
    let doc = root(&reading);
    assert_eq!(
        doc.get("b")
            .and_then(Node::as_scalar)
            .map(|s| s.text.as_str()),
        Some("1")
    );
    assert_eq!(
        doc.get("c")
            .and_then(Node::as_scalar)
            .map(|s| s.text.as_str()),
        Some("2")
    );
    assert!(doc.get("a").is_some_and(|n| n.templated));
    assert!(!doc.get("b").expect("b").templated);
}

#[test]
fn without_templating_the_text_is_read_as_it_stands() {
    let reading = yaml("a: 1\nb: \"{{ not.an.action }}\"\n");
    assert!(reading.templates.is_empty());
    assert!(reading.is_whole(), "{reading:?}");
    assert_eq!(str_at(root(&reading), "b"), Some("{{ not.an.action }}"));
}

// --- JSON ---

#[test]
fn json_values_strings_and_comments() {
    let source = "{\"name\": \"api\", \"ports\": [80, 8080.5], \"on\": true, \"none\": null, \"esc\": \"a\\u0041\\n\", // c\n \"nested\": {\"k\": []}}
[1]";
    let reading = load(Dialect::Json, source);
    assert!(reading.is_whole(), "{reading:?}");
    assert_eq!(reading.documents.len(), 2);
    let doc = root(&reading);
    assert_eq!(str_at(doc, "name"), Some("api"));
    let ports = doc.get("ports").and_then(Node::as_seq).expect("ports");
    let kinds: Vec<_> = ports
        .iter()
        .filter_map(Node::as_scalar)
        .map(|s| s.kind)
        .collect();
    assert_eq!(kinds, [ScalarKind::Int, ScalarKind::Float]);
    assert_eq!(doc.get("on").and_then(Node::as_bool), Some(true));
    assert!(doc.get("none").is_some_and(Node::is_null));
    assert_eq!(str_at(doc, "esc"), Some("aA\n"));
    assert!(
        doc.at("nested.k")
            .and_then(Node::as_seq)
            .is_some_and(<[Node]>::is_empty)
    );
    assert_eq!(
        text_of(source, &doc.get("name").expect("name").span),
        "\"api\""
    );
}

#[test]
fn a_json_error_is_a_line_range() {
    let reading = load(Dialect::Json, "{\n  \"a\": \n}");
    assert!(!reading.errors.is_empty(), "{reading:?}");
}

// --- TOML ---

const TOML: &str = "title = \"x\"
[server]
host = \"h\"
port = 8080
[[servers]]
name = \"a\"
[[servers]]
name = \"b\"
[a.b]
c = 1.5
";

#[test]
fn toml_tables_arrays_of_tables_and_spans_in_source_order() {
    let reading = load(Dialect::Toml, TOML);
    assert!(reading.is_whole(), "{reading:?}");
    let doc = root(&reading);
    let keys: Vec<&str> = doc.entries().iter().map(|e| e.key.text.as_str()).collect();
    assert_eq!(keys, ["title", "server", "servers", "a"]);
    assert_eq!(str_at(doc, "title"), Some("x"));
    let port = doc.at("server.port").expect("a port");
    assert_eq!(
        port.as_scalar().map(|s| (s.text.as_str(), s.kind)),
        Some(("8080", ScalarKind::Int))
    );
    assert_eq!(text_of(TOML, &port.span), "8080");
    assert_eq!(port.span.lines, (4, 4));
    let servers = doc.get("servers").and_then(Node::as_seq).expect("servers");
    let names: Vec<_> = servers
        .iter()
        .filter_map(|s| s.get("name"))
        .filter_map(Node::as_str)
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(
        doc.at("a.b.c").and_then(Node::as_scalar).map(|s| s.kind),
        Some(ScalarKind::Float)
    );
}

#[test]
fn a_toml_error_is_recovered_from_and_named() {
    let reading = load(Dialect::Toml, "ok = 1\nbad =\nnext = 2\n");
    assert!(!reading.errors.is_empty(), "{reading:?}");
    assert!(!reading.is_whole());
}

// --- INI ---

const INI: &str = "; comment
top = 1

[core]
name = diffctx
url: http://x/y=z
multi = first
    second

    third
flag
[ spaced ]
k=v
";

#[test]
fn ini_sections_delimiters_continuations_and_keys_without_values() {
    let reading = load(Dialect::Ini, INI);
    assert!(reading.is_whole(), "{reading:?}");
    let doc = root(&reading);
    let keys: Vec<&str> = doc.entries().iter().map(|e| e.key.text.as_str()).collect();
    assert_eq!(keys, ["top", "core", " spaced "]);
    assert_eq!(str_at(doc, "top"), Some("1"));
    let core = doc.get("core").expect("a section");
    assert_eq!(str_at(core, "name"), Some("diffctx"));
    // The first delimiter on the line splits it: the value keeps the rest.
    assert_eq!(str_at(core, "url"), Some("http://x/y=z"));
    assert_eq!(str_at(core, "multi"), Some("first\nsecond\n\nthird"));
    assert!(core.get("flag").is_some_and(Node::is_null));
    assert_eq!(
        str_at(doc.get(" spaced ").expect("a section"), "k"),
        Some("v")
    );
    let name = core.get("name").expect("name");
    assert_eq!(text_of(INI, &name.span), "diffctx");
    assert_eq!(name.span.lines, (5, 5));
}

#[test]
fn an_ini_section_that_stands_twice_is_two_entries_and_an_open_header_is_an_error() {
    let reading = load(Dialect::Ini, "[a]\nx=1\nx=2\n[a]\ny=3\n");
    let doc = root(&reading);
    assert_eq!(doc.get_all("a").count(), 2);
    let first = doc.entries()[0].value.clone();
    assert_eq!(first.get_all("x").count(), 2);
    assert_eq!(str_at(&first, "x"), Some("2"));
    let broken = load(Dialect::Ini, "[core\nk=v\n");
    assert_eq!(broken.errors, [(1, 1)]);
}

// --- objects, references, pointers ---

const DEPLOYMENT: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  namespace: shop
spec:
  template:
    spec:
      serviceAccountName: runner
      volumes:
        - name: cfg
          configMap:
            name: app-config
        - name: tls
          secret:
            secretName: app-tls
        - name: numbered
          configMap:
            name: 80
        - name: scratch
          emptyDir: {}
      containers:
        - name: app
          env:
            - name: MODE
              valueFrom:
                configMapKeyRef:
                  name: app-config
                  key: mode
            - name: PLAIN
              value: x
";

const ROWS: &[Row] = &[
    Row::new(
        &["Deployment"],
        "spec.template.spec.volumes[].configMap.name",
        "ConfigMap",
    ),
    Row::new(
        &["Deployment"],
        "spec.template.spec.volumes[].secret.secretName",
        "Secret",
    ),
    Row::new(
        &[],
        "spec.template.spec.containers[].env[].valueFrom.configMapKeyRef.name",
        "ConfigMap",
    )
    .key("key"),
    Row::new(
        &["Deployment"],
        "spec.template.spec.serviceAccountName",
        "ServiceAccount",
    ),
    Row::new(
        &["StatefulSet"],
        "spec.template.spec.serviceAccountName",
        "NotThisKind",
    ),
    Row::new(&["RoleBinding"], "subjects[].name", "ServiceAccount").namespace("namespace"),
    Row::new(&["RoleBinding"], "roleRef.name", "").kind_from("kind"),
];

#[test]
fn the_table_says_which_fields_are_references_and_the_rest_are_not() {
    let reading = yaml(DEPLOYMENT);
    assert!(reading.is_whole(), "{reading:?}");
    let found = objects(&reading);
    assert_eq!(found.len(), 1);
    let refs = extract(&found[0], &Scope::Module, ROWS);
    let seen: Vec<(&str, &str, Option<&str>)> = refs
        .iter()
        .map(|r| {
            (
                r.target_kind.as_str(),
                r.target_name.as_str(),
                r.key.as_deref(),
            )
        })
        .collect();
    // `name: cfg`, `name: app` and `name: MODE` are no references; `name: 80` is
    // a number, a row for another kind does not apply.
    assert_eq!(
        seen,
        [
            ("ConfigMap", "app-config", None),
            ("Secret", "app-tls", None),
            ("ConfigMap", "app-config", Some("mode")),
            ("ServiceAccount", "runner", None),
        ]
    );
    for r in &refs {
        assert_eq!(text_of(DEPLOYMENT, &r.site), r.target_name);
        assert_eq!(r.from.namespace.as_deref(), Some("shop"));
        assert_eq!(r.target_namespace, None);
        assert_eq!(r.strength, Strength::Typed);
        assert!(!r.templated);
    }
}

#[test]
fn a_row_can_read_the_namespace_that_stands_beside_the_name() {
    let reading = yaml(
        "apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: rb
subjects:
  - kind: ServiceAccount
    name: runner
    namespace: ci
  - kind: ServiceAccount
    name: other
",
    );
    let found = objects(&reading);
    let refs = extract(&found[0], &Scope::Module, ROWS);
    let seen: Vec<(&str, Option<&str>)> = refs
        .iter()
        .map(|r| (r.target_name.as_str(), r.target_namespace.as_deref()))
        .collect();
    assert_eq!(seen, [("runner", Some("ci")), ("other", None)]);
}

#[test]
fn a_reference_written_by_a_template_is_marked_not_dropped() {
    let reading = helm(
        "apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
spec:
  template:
    spec:
      volumes:
        - name: cfg
          configMap:
            name: {{ .Values.cm }}
        - name: lit
          configMap:
            name: literal
",
    );
    let found = objects(&reading);
    assert!(!found[0].object.templated);
    let refs = extract(&found[0], &Scope::Module, ROWS);
    let seen: Vec<(&str, bool)> = refs
        .iter()
        .map(|r| (r.target_name.as_str(), r.templated))
        .collect();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], ("literal", false));
    assert!(seen[0].1);
}

#[test]
fn a_field_path_is_keys_and_every_item() {
    assert_eq!(
        FieldPath::parse("a.b[].c").expect("a path").segments(),
        [
            Segment::Key("a".into()),
            Segment::Key("b".into()),
            Segment::Each,
            Segment::Key("c".into())
        ]
    );
    assert!(FieldPath::parse("").is_none());
    assert!(FieldPath::parse("a..b").is_none());
    assert!(FieldPath::parse("[]").is_none());
}

fn pointer(kind: PointerKind, text: &str) -> SourcePointer {
    SourcePointer {
        site: Span::default(),
        kind,
        text: text.to_string(),
        templated: false,
    }
}

#[test]
fn a_pointer_resolves_against_the_directory_of_its_file_as_text() {
    let path = |text: &str, base: &str| pointer(PointerKind::Path, text).resolve(base);
    let local = |s: &str| Resolved::Local(s.to_string());
    assert_eq!(path("../base", "overlays/prod"), local("overlays/base"));
    assert_eq!(
        path("./patch.yaml", "overlays/prod"),
        local("overlays/prod/patch.yaml")
    );
    assert_eq!(path(".", "apps"), local("apps"));
    assert_eq!(path("a//b/./c/", ""), local("a/b/c"));
    assert_eq!(path("../../../x", "a/b"), Resolved::Escapes);
    assert_eq!(
        path("/apps/x", "deploy"),
        Resolved::Rooted("apps/x".to_string())
    );
    for external in [
        "https://example.com/x.git",
        "git@host:org/r.git",
        "git::https://example.com/x.git",
        "github.com/org/repo//dir?ref=v1",
    ] {
        assert_eq!(path(external, "a"), Resolved::External, "{external}");
    }
    for computed in ["$values/vals.yaml", "${DIR}/x", "$(pwd)/x", "~/x", ""] {
        assert_eq!(path(computed, "a"), Resolved::Computed, "{computed:?}");
    }
}

#[test]
fn a_repository_an_image_and_a_template_are_never_a_path() {
    assert_eq!(
        pointer(PointerKind::Repo, "./local").resolve(""),
        Resolved::External
    );
    assert_eq!(
        pointer(PointerKind::Image, "app:1").resolve(""),
        Resolved::External
    );
    let mut templated = pointer(PointerKind::Path, "____");
    templated.templated = true;
    assert_eq!(templated.resolve(""), Resolved::Computed);
}

#[test]
fn a_chart_is_a_directory_only_when_it_is_written_as_one() {
    let chart = |text: &str, base: &str| pointer(PointerKind::Chart, text).resolve(base);
    assert_eq!(
        chart("./charts/app", "deploy"),
        Resolved::Local("deploy/charts/app".to_string())
    );
    assert_eq!(
        chart("file://../common", "charts/app"),
        Resolved::Local("charts/common".to_string())
    );
    assert_eq!(chart("stable/nginx", "deploy"), Resolved::External);
    assert_eq!(
        chart("oci://registry.example/chart", "deploy"),
        Resolved::External
    );
    assert_eq!(directory_of("a/b/c.yaml"), "a/b");
    assert_eq!(directory_of("c.yaml"), "");
}

#[test]
fn the_dialect_is_the_file_name() {
    for (path, dialect) in [
        ("k/a.yaml", Some(Dialect::Yaml)),
        ("K/A.YML", Some(Dialect::Yaml)),
        ("x.json", Some(Dialect::Json)),
        ("Cargo.toml", Some(Dialect::Toml)),
        ("tox.ini", Some(Dialect::Ini)),
        ("setup.cfg", Some(Dialect::Ini)),
        ("a/.editorconfig", Some(Dialect::Ini)),
        (".yaml", None),
        ("nginx.conf", None),
        ("Makefile", None),
        ("main.tf", None),
    ] {
        assert_eq!(Dialect::of_path(path), dialect, "{path}");
    }
}

// --- bounds, determinism, the run's deadline ---

fn walk(node: &Node, visit: &mut impl FnMut(&Node)) {
    visit(node);
    match &node.value {
        Value::Seq(items) => items.iter().for_each(|item| walk(item, visit)),
        Value::Map(entries) => entries.iter().for_each(|e| walk(&e.value, visit)),
        Value::Null | Value::Scalar(_) => {}
    }
}

#[test]
fn a_read_of_any_prefix_of_a_file_stays_inside_the_prefix() {
    let fixtures: [(Dialect, Options, &str); 6] = [
        (Dialect::Yaml, Options::default(), MANIFESTS),
        (Dialect::Yaml, Options::default(), DEPLOYMENT),
        (
            Dialect::Yaml,
            Options {
                templating: Templating::Go,
            },
            CHART_TEMPLATE,
        ),
        (
            Dialect::Json,
            Options::default(),
            "{\"a\": [1, {\"b\": \"c\\n\"}], \"d\": null}",
        ),
        (Dialect::Toml, Options::default(), TOML),
        (Dialect::Ini, Options::default(), INI),
    ];
    for (dialect, options, text) in fixtures {
        for cut in 0..=text.len() {
            let reading = read(
                dialect,
                &text.as_bytes()[..cut],
                options,
                &RunBudget::default(),
            );
            for document in &reading.documents {
                walk(document, &mut |node| {
                    let span = &node.span;
                    assert!(
                        span.bytes.start <= span.bytes.end && span.bytes.end as usize <= cut,
                        "{dialect:?} cut at {cut}: {span:?}"
                    );
                    assert!(
                        span.lines.0 <= span.lines.1,
                        "{dialect:?} cut at {cut}: {span:?}"
                    );
                });
            }
            for t in &reading.templates {
                assert!(t.span.bytes.end as usize <= cut, "{dialect:?} cut at {cut}");
            }
        }
    }
}

#[test]
fn the_same_bytes_give_the_same_reading_and_a_reading_survives_serde() {
    let first = helm(CHART_TEMPLATE);
    assert_eq!(first, helm(CHART_TEMPLATE));
    let json = serde_json::to_string(&first).expect("serializes");
    let back: Reading = serde_json::from_str(&json).expect("reads back");
    assert_eq!(first, back);
}

#[test]
fn an_expired_run_says_so_instead_of_returning_a_clean_file() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let many: String = (0..300).map(|i| format!("k{i} = {i}\n")).collect();
    let yaml_many: String = (0..300).map(|i| format!("k{i}: {i}\n")).collect();
    let json_many = format!(
        "[{}]",
        (0..300)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    for (dialect, text) in [
        (Dialect::Yaml, yaml_many.as_str()),
        (Dialect::Json, json_many.as_str()),
        (Dialect::Toml, many.as_str()),
        (Dialect::Ini, many.as_str()),
    ] {
        let reading = read(dialect, text.as_bytes(), Options::default(), &budget);
        assert!(
            reading.flaws.contains(&Flaw::Deadline),
            "{dialect:?}: {:?}",
            reading.flaws
        );
        assert!(!reading.is_whole());
    }
}

#[test]
fn nesting_past_the_depth_is_named_not_followed() {
    let deep = format!("{}1{}", "[".repeat(500), "]".repeat(500));
    let reading = load(Dialect::Json, &deep);
    assert!(
        reading.flaws.contains(&Flaw::TooDeep),
        "{:?}",
        reading.flaws
    );
}
