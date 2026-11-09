//! The configuration extractor, fact kind by fact kind, on YAML small enough to
//! read the right answer off. Nothing here runs diffctx to learn an answer.

use _diffctx::facts::config::{ObjectId, Strength, identity_of};
use _diffctx::facts::lang::config::ConfigExtractor;
use _diffctx::facts::{BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Scope};
use _diffctx::snapshot::{Loss, RunBudget};

fn extract(source: &str) -> BlobFacts {
    ConfigExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<&Def> {
    blob.defs().collect()
}

const MANIFESTS: &str = "apiVersion: apps/v1
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
      containers:
        - name: app
          env:
            - name: MODE
              valueFrom:
                configMapKeyRef:
                  name: app-config
                  key: mode
---
apiVersion: v1
kind: Service
metadata:
  name: api
spec:
  storageClassName: fast
";

#[test]
fn an_object_is_a_definition_keyed_by_kind_group_name_and_namespace() {
    let blob = extract(MANIFESTS);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
    let found = defs(&blob);
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|d| d.kind == DefKind::Object));
    assert_eq!(found[0].local.name.as_str(), "Deployment.apps/api");
    assert_eq!(
        found[0].local.disambig,
        Some(Disambig::Cfg("ns:shop".to_string()))
    );
    assert_eq!(found[1].local.name.as_str(), "Service/api");
    assert_eq!(found[1].local.disambig, None);
    assert_eq!(
        identity_of(&found[0].local),
        Some(ObjectId {
            group: "apps".to_string(),
            kind: "Deployment".to_string(),
            namespace: Some("shop".to_string()),
            name: "api".to_string(),
        })
    );
    assert_eq!(found[0].span.lines, (1, 21));
    assert_eq!(found[0].name_span.lines, (4, 4));
}

#[test]
fn a_twin_in_one_file_is_told_apart_and_round_trips_to_its_identity() {
    let blob = extract(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n",
    );
    let found = defs(&blob);
    assert_eq!(found.len(), 2);
    assert_ne!(found[0].local, found[1].local);
    assert_eq!(identity_of(&found[0].local), identity_of(&found[1].local));
}

#[test]
fn only_a_change_of_content_moves_the_body_hash() {
    let hash = |source: &str| defs(&extract(source))[0].body_hash;
    let text = |source: &str| defs(&extract(source))[0].text_hash;
    let base = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\ndata:\n  mode: fast\n";
    let layout = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n# a note\ndata:\n    mode:    fast\n";
    let edited = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\ndata:\n  mode: slow\n";
    assert_eq!(hash(base), hash(layout));
    assert_ne!(text(base), text(layout));
    assert_ne!(hash(base), hash(edited));
}

#[test]
fn the_fields_of_the_table_are_typed_references_and_a_suffix_alone_is_a_candidate() {
    let blob = extract(MANIFESTS);
    let refs: Vec<_> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ConfigRef(r) => Some(r),
            _ => None,
        })
        .collect();
    let seen: Vec<(Strength, &str, &str, Option<&str>)> = refs
        .iter()
        .map(|r| {
            (
                r.strength,
                r.target_kind.as_str(),
                r.target_name.as_str(),
                r.key.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (Strength::Typed, "ServiceAccount", "runner", None),
            (Strength::Typed, "ConfigMap", "app-config", None),
            (Strength::Typed, "ConfigMap", "app-config", Some("mode")),
            (Strength::Candidate, "", "fast", None),
        ]
    );
    // The reference belongs to the object that writes it.
    let deployment = &defs(&blob)[0].local;
    assert!(
        refs[..3]
            .iter()
            .all(|r| r.enclosing == Scope::Def(deployment.clone()))
    );
    // `name: cfg`, `name: app` and `name: MODE` name nothing.
    assert!(
        refs.iter()
            .all(|r| !["cfg", "app", "MODE"].contains(&r.target_name.as_str()))
    );
}

#[test]
fn a_place_a_tool_names_is_a_pointer_whether_it_is_of_the_repository_or_not() {
    let places = |source: &str| -> Vec<(String, bool)> {
        extract(source)
            .facts
            .iter()
            .filter_map(|f| match f {
                Fact::ConfigPointer(p) => Some((p.text.clone(), p.is_local())),
                _ => None,
            })
            .collect()
    };
    let own = |text: &str| (text.to_string(), true);
    let outside = |text: &str| (text.to_string(), false);
    assert_eq!(
        places(
            "resources:
  - deploy.yaml
  - ../base
  - https://example.com/x.yaml
  - github.com/org/repo//dir?ref=v1
patches:
  - path: patch.yaml
configMapGenerator:
  - name: c
    files:
      - key=settings.properties
"
        ),
        [
            own("deploy.yaml"),
            own("../base"),
            outside("https://example.com/x.yaml"),
            outside("github.com/org/repo//dir?ref=v1"),
            own("patch.yaml"),
            own("settings.properties"),
        ]
    );
    assert_eq!(
        places(
            "apiVersion: v2
name: app
version: 1.0.0
dependencies:
  - name: common
    repository: file://../common
  - name: redis
    repository: https://charts.example.com
"
        ),
        [
            own("file://../common"),
            outside("https://charts.example.com")
        ]
    );
}

const SELECTED: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: shop
  labels:
    team: front
spec:
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
        tier: ui
    spec:
      containers: []
---
apiVersion: v1
kind: Service
metadata:
  name: web
spec:
  selector:
    app: web
---
apiVersion: v1
kind: Service
metadata:
  name: everything
spec:
  selector: {}
---
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: web
spec:
  selector:
    matchLabels:
      app: web
      tier: ui
";

#[test]
fn the_labels_of_an_object_are_definitions_and_a_selector_is_a_fact_of_its_own() {
    let blob = extract(SELECTED);
    let found = defs(&blob);
    let names: Vec<(String, DefKind)> = found
        .iter()
        .map(|d| {
            let mut path: Vec<&str> = d.local.container.iter().map(|n| n.as_str()).collect();
            path.push(d.local.name.as_str());
            (path.join(" > "), d.kind)
        })
        .collect();
    assert_eq!(
        names,
        [
            ("Deployment.apps/web".to_string(), DefKind::Object),
            ("Deployment.apps/web > labels".to_string(), DefKind::Field),
            (
                "Deployment.apps/web > pod-labels".to_string(),
                DefKind::Field
            ),
            ("Service/web".to_string(), DefKind::Object),
            ("Service/everything".to_string(), DefKind::Object),
            (
                "PodDisruptionBudget.policy/web".to_string(),
                DefKind::Object
            ),
        ]
    );
    // The labels carry the namespace of their object, so two objects of one
    // name in two namespaces keep two label definitions.
    assert_eq!(found[1].local.disambig, found[0].local.disambig);
    let pairs: Vec<(&str, Vec<(&str, &str)>)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ConfigLabels(l) => Some((
                match &l.enclosing {
                    Scope::Def(local) => local.name.as_str(),
                    Scope::Module | Scope::Callback { .. } => "?",
                },
                l.pairs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("labels", vec![("team", "front")]),
            ("pod-labels", vec![("app", "web"), ("tier", "ui")]),
        ]
    );
    let selectors: Vec<(&str, Vec<(&str, &str)>, Option<&str>)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ConfigSelector(s) => Some((
                s.field.as_str(),
                s.pairs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect(),
                s.namespace.as_deref(),
            )),
            _ => None,
        })
        .collect();
    // The empty selector of `everything` selects all and says nothing.
    assert_eq!(
        selectors,
        [
            (
                "spec.selector.matchLabels",
                vec![("app", "web")],
                Some("shop")
            ),
            ("spec.selector", vec![("app", "web")], None),
            (
                "spec.selector.matchLabels",
                vec![("app", "web"), ("tier", "ui")],
                None
            ),
        ]
    );
}

#[test]
fn only_an_edit_of_the_labels_moves_the_hash_of_their_definition() {
    let labels_hash = |source: &str| {
        defs(&extract(source))
            .iter()
            .find(|d| d.local.name.as_str() == "pod-labels")
            .map(|d| d.body_hash)
    };
    let base = SELECTED;
    let replicas = SELECTED.replace(
        "spec:\n  selector:\n    matchLabels:\n      app: web\n  template",
        "spec:\n  replicas: 3\n  selector:\n    matchLabels:\n      app: web\n  template",
    );
    let relabelled = SELECTED.replace("tier: ui", "tier: api");
    assert!(labels_hash(base).is_some());
    assert_eq!(labels_hash(base), labels_hash(&replicas));
    assert_ne!(labels_hash(base), labels_hash(&relabelled));
}

#[test]
fn a_file_with_no_object_has_no_definition_and_a_template_has_a_parse_error() {
    let plain = extract("on: push\njobs:\n  build:\n    runs-on: ubuntu\n");
    assert!(defs(&plain).is_empty());
    assert!(plain.losses.is_empty());
    let template = extract(
        "metadata:\n  name: {{ .Release.Name }}-svc\n  {{- if .Values.x }}\n  a: b\n  {{- end }}\n",
    );
    assert!(
        template
            .losses
            .iter()
            .any(|l| matches!(l, Loss::ParseError { .. })),
        "{:?}",
        template.losses
    );
}
