//! The Helm extractors, fact kind by fact kind, on files small enough to read the
//! right answer off. Nothing here runs diffctx to learn an answer.

use _diffctx::facts::config::{ConfigUse, UseOf};
use _diffctx::facts::lang::config::ConfigExtractor;
use _diffctx::facts::lang::helm::{HelmTemplateExtractor, HelmValuesExtractor};
use _diffctx::facts::{BlobFacts, Def, DefKind, Extractor, Fact, Language, Scope};
use _diffctx::snapshot::{RepoPath, RunBudget};

fn language_of(path: &str) -> Option<Language> {
    Language::from_path(&RepoPath::new(path).expect("a path"))
}

fn template(source: &str) -> BlobFacts {
    HelmTemplateExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn values(source: &str) -> BlobFacts {
    HelmValuesExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<&Def> {
    blob.defs().collect()
}

fn uses(blob: &BlobFacts) -> Vec<&ConfigUse> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::ConfigUse(u) => Some(u),
            _ => None,
        })
        .collect()
}

#[test]
fn the_language_of_a_chart_file_comes_from_its_path() {
    for (path, language) in [
        ("chart/templates/deploy.yaml", Some(Language::HelmTemplate)),
        ("templates/deploy.yml", Some(Language::HelmTemplate)),
        (
            "charts/app/templates/_helpers.tpl",
            Some(Language::HelmTemplate),
        ),
        ("_helpers.tpl", Some(Language::HelmTemplate)),
        ("chart/values.yaml", Some(Language::HelmValues)),
        ("env/values-prod.yaml", Some(Language::HelmValues)),
        ("env/prod-values.yml", Some(Language::HelmValues)),
        ("chart/ci/test.values.yaml", Some(Language::HelmValues)),
        ("deploy/app.yaml", Some(Language::Config)),
        ("docs/templates/readme.md", None),
        ("src/templates.py", Some(Language::Python)),
        ("notes.tpl", None),
    ] {
        assert_eq!(language_of(path), language, "{path}");
    }
}

const HELPERS: &str = "{{- define \"chart.waitForDb\" -}}
wait-for-db
{{- end -}}

{{- define \"chart.init\" -}}
{{ include \"chart.waitForDb\" . }}
{{- end -}}
";

#[test]
fn a_named_template_is_a_definition_from_its_define_to_its_end() {
    let blob = template(HELPERS);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
    let found = defs(&blob);
    let seen: Vec<(&str, DefKind, (u32, u32))> = found
        .iter()
        .map(|d| (d.local.name.as_str(), d.kind, d.span.lines))
        .collect();
    assert_eq!(
        seen,
        [
            ("chart.waitForDb", DefKind::Function, (1, 3)),
            ("chart.init", DefKind::Function, (5, 7)),
        ]
    );
    // The include is the call of the named template it sits in.
    let included = uses(&blob);
    assert_eq!(included.len(), 1);
    assert_eq!(included[0].of, UseOf::Include);
    assert_eq!(included[0].name, "chart.waitForDb");
    assert_eq!(included[0].site.lines, (6, 6));
    assert_eq!(included[0].enclosing, Scope::Def(found[1].local.clone()));
}

#[test]
fn only_a_change_of_the_named_template_moves_its_hash() {
    let hash = |source: &str| defs(&template(source))[0].body_hash;
    let layout = HELPERS.replace("wait-for-db", "wait-for-db   ");
    let edited = HELPERS.replace("wait-for-db", "wait-for-database");
    assert_eq!(hash(HELPERS), hash(&layout));
    assert_ne!(hash(HELPERS), hash(&edited));
}

const DEPLOYMENT: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ include \"app.fullname\" . }}
spec:
  {{- if .Values.autoscaling.enabled }}
  replicas: {{ .Values.replicaCount }}
  {{- end }}
  template:
    spec:
      serviceAccountName: runner
      containers:
        - name: app
          image: \"{{ .Values.image.repository }}:{{ index .Values \"image\" \"tag\" }}\"
          env: {{ toYaml .Values.env | nindent 12 }}
";

#[test]
fn a_template_reads_values_by_their_literal_keys_and_names_what_it_includes() {
    let blob = template(DEPLOYMENT);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
    let seen: Vec<(UseOf, &str, u32)> = uses(&blob)
        .iter()
        .map(|u| (u.of, u.name.as_str(), u.site.lines.0))
        .collect();
    assert_eq!(
        seen,
        [
            (UseOf::Include, "app.fullname", 4),
            (UseOf::Values, "autoscaling.enabled", 6),
            (UseOf::Values, "replicaCount", 7),
            (UseOf::Values, "image.repository", 14),
            (UseOf::Values, "image.tag", 14),
            (UseOf::Values, "env", 15),
        ]
    );
    assert!(uses(&blob).iter().all(|u| u.enclosing == Scope::Module));
}

#[test]
fn an_object_whose_name_a_template_writes_is_no_definition_and_its_literal_references_stay() {
    let blob = template(DEPLOYMENT);
    assert!(defs(&blob).is_empty());
    let refs: Vec<(&str, &str)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ConfigRef(r) => Some((r.target_kind.as_str(), r.target_name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(refs, [("ServiceAccount", "runner")]);
}

#[test]
fn the_keys_of_a_values_file_are_definitions_at_every_depth() {
    let blob = values(
        "replicaCount: 2
image:
  repository: nginx
  tag: \"1.0\"
env:
  - name: A
",
    );
    let found = defs(&blob);
    let seen: Vec<(String, DefKind, (u32, u32))> = found
        .iter()
        .map(|d| {
            let mut path: Vec<&str> = d.local.container.iter().map(|n| n.as_str()).collect();
            path.push(d.local.name.as_str());
            (path.join("."), d.kind, d.span.lines)
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("replicaCount".to_string(), DefKind::Field, (1, 1)),
            ("image".to_string(), DefKind::Field, (2, 4)),
            ("image.repository".to_string(), DefKind::Field, (3, 3)),
            ("image.tag".to_string(), DefKind::Field, (4, 4)),
            ("env".to_string(), DefKind::Field, (5, 6)),
        ]
    );
}

#[test]
fn an_edit_of_a_leaf_moves_the_key_and_every_key_above_it_and_no_other() {
    let hashes = |source: &str| -> Vec<(String, u64)> {
        defs(&values(source))
            .iter()
            .map(|d| (d.local.name.to_string(), d.body_hash))
            .collect()
    };
    let before = hashes("a: 1\nimage:\n  tag: x\n  repo: y\n");
    let after = hashes("a: 1\nimage:\n  tag: z\n  repo: y\n");
    let moved: Vec<&str> = before
        .iter()
        .zip(&after)
        .filter(|(b, a)| b.1 != a.1)
        .map(|(b, _)| b.0.as_str())
        .collect();
    assert_eq!(moved, ["image", "tag"]);
}

#[test]
fn a_helmfile_release_and_an_argo_source_are_releases_of_a_chart() {
    let releases = |source: &str| -> Vec<_> {
        ConfigExtractor
            .extract(source.as_bytes(), &RunBudget::default())
            .facts
            .into_iter()
            .filter_map(|f| match f {
                Fact::ConfigRelease(r) => Some(r),
                _ => None,
            })
            .collect()
    };
    let helmfile = releases(
        "releases:
  - name: demo
    chart: ./chart
    values:
      - envs/values-prod.yaml
      - inline: true
",
    );
    assert_eq!(helmfile.len(), 1);
    assert_eq!(helmfile[0].name.as_deref(), Some("demo"));
    assert_eq!(
        helmfile[0].chart.as_ref().map(|c| c.text.as_str()),
        Some("./chart")
    );
    assert_eq!(
        helmfile[0]
            .values
            .iter()
            .map(|v| v.text.as_str())
            .collect::<Vec<_>>(),
        ["envs/values-prod.yaml"]
    );
    assert!(!helmfile[0].values_from_chart);
    let argo = releases(
        "apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: demo
spec:
  source:
    repoURL: https://example.com/repo.git
    path: deploy/chart
    helm:
      valueFiles:
        - values-prod.yaml
",
    );
    assert_eq!(argo.len(), 1);
    // Relative to the root of the source repository.
    assert_eq!(
        argo[0].chart.as_ref().map(|c| c.text.as_str()),
        Some("/deploy/chart")
    );
    assert_eq!(argo[0].values.len(), 1);
    assert!(argo[0].values_from_chart);
}
