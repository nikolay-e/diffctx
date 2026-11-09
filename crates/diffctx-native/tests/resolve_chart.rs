//! A Helm chart on real git repositories: what a template reads of `.Values` lands
//! on only inside the chart and the release that gives it values, and a named
//! template is included only within its own chart tree. The expectations are
//! read off the fixtures; nothing here runs diffctx to learn an answer.

mod snapshot_fixture;

use _diffctx::facts::{DefKey, DefKind, FactsCache};
use _diffctx::resolve::{Binding, ConfigResolver, Relation, Resolution};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag};
use snapshot_fixture::Repo;

struct Fixture {
    _repo: Repo,
    head: ConfigResolver,
}

fn open(files: &[(&str, &str)]) -> Fixture {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::At(Side::Tree(head)),
        RunBudget::default(),
    )
    .expect("an analysis context");
    let resolver = ConfigResolver::open(&ctx, SideTag::Head, &FactsCache::disabled())
        .expect("the head resolves");
    Fixture {
        _repo: repo,
        head: resolver,
    }
}

impl Fixture {
    fn where_is(&self, key: &DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition");
        assert_ne!(found.kind, DefKind::Object);
        let mut path: Vec<&str> = key.local.container.iter().map(|n| n.as_str()).collect();
        path.push(key.local.name.as_str());
        format!("{} in {}", path.join("."), found.file)
    }

    /// What each use of a file lands on, by line, in order.
    fn lands(&self, file: &str) -> Vec<(u32, String)> {
        self.head
            .bindings()
            .iter()
            .filter(|b| b.file.as_str() == file && b.relation != Relation::Imports)
            .map(|b| (b.site.lines.0, self.describe(b)))
            .collect()
    }

    fn describe(&self, binding: &Binding) -> String {
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.where_is(target),
            Resolution::Candidate { targets, reason } => {
                let mut names: Vec<String> = targets.iter().map(|t| self.where_is(t)).collect();
                names.sort();
                format!("candidate({reason:?}) {}", names.join(" | "))
            }
            Resolution::Module { file, .. } => format!("module {file}"),
            Resolution::Unresolved { reason } => format!("unresolved({reason:?})"),
        }
    }
}

const CHART: &str = "apiVersion: v2\nname: demo\nversion: 0.1.0\n";
const VALUES: &str = "image:\n  tag: \"1\"\nreplicas: 1\n";
const DEPLOY: &str = "apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
spec:
  template:
    spec:
      containers:
        - name: app
          image: \"app:{{ .Values.image.tag }}\"
";

#[test]
fn a_chart_default_is_a_candidate_for_a_read_when_no_release_is_written_down() {
    let fixture = open(&[
        ("chart/Chart.yaml", CHART),
        ("chart/values.yaml", VALUES),
        ("chart/templates/deploy.yaml", DEPLOY),
    ]);
    assert_eq!(
        fixture.lands("chart/templates/deploy.yaml"),
        [(
            10,
            "candidate(NoRelease) image.tag in chart/values.yaml".to_string()
        )]
    );
    assert_eq!(fixture.head.unbound_values(), 0);
}

#[test]
fn a_values_file_a_release_gives_the_chart_is_read_as_resolved() {
    let fixture = open(&[
        (
            "helmfile.yaml",
            "releases:\n  - name: demo\n    chart: ./chart\n    values:\n      - envs/values-prod.yaml\n",
        ),
        ("chart/Chart.yaml", CHART),
        ("chart/values.yaml", VALUES),
        ("envs/values-prod.yaml", "image:\n  tag: \"2\"\n"),
        ("envs/values-unused.yaml", "image:\n  tag: \"3\"\n"),
        ("chart/templates/deploy.yaml", DEPLOY),
    ]);
    assert_eq!(
        fixture.lands("chart/templates/deploy.yaml"),
        [
            (10, "image.tag in envs/values-prod.yaml".to_string()),
            (
                10,
                "candidate(NoRelease) image.tag in chart/values.yaml".to_string()
            ),
        ]
    );
    // `values-unused.yaml` is named by no release and held by no chart.
    assert_eq!(fixture.head.unbound_values(), 1);
}

#[test]
fn an_argo_source_gives_its_chart_the_value_files_written_relative_to_it() {
    let fixture = open(&[
        (
            "apps/demo.yaml",
            "apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: demo
spec:
  source:
    repoURL: https://example.com/repo.git
    path: chart
    helm:
      valueFiles:
        - values-prod.yaml
",
        ),
        ("chart/Chart.yaml", CHART),
        ("chart/values-prod.yaml", "image:\n  tag: \"2\"\n"),
        ("chart/templates/deploy.yaml", DEPLOY),
    ]);
    assert_eq!(
        fixture.lands("chart/templates/deploy.yaml"),
        [(10, "image.tag in chart/values-prod.yaml".to_string())]
    );
    assert_eq!(fixture.head.unbound_values(), 0);
}

#[test]
fn a_key_no_values_file_sets_is_a_read_of_nothing_and_a_parent_key_is_the_nearest() {
    let fixture = open(&[
        ("chart/Chart.yaml", CHART),
        ("chart/values.yaml", "image:\n  repo: x\n"),
        (
            "chart/templates/a.yaml",
            "a: {{ .Values.image.tag }}\nb: {{ .Values.missing }}\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("chart/templates/a.yaml"),
        [
            (
                1,
                "candidate(NoRelease) image in chart/values.yaml".to_string()
            ),
            (2, "unresolved(NoMatch)".to_string()),
        ]
    );
}

const HELPERS: &str = "{{- define \"chart.waitForDb\" -}}
wait-for-db
{{- end -}}

{{- define \"chart.init\" -}}
{{ include \"chart.waitForDb\" . }}
{{- end -}}
";

#[test]
fn an_include_lands_on_the_named_template_of_its_own_chart_tree_alone() {
    let other = "{{- define \"chart.waitForDb\" -}}\nother\n{{- end -}}\n";
    let fixture = open(&[
        ("charts/app/Chart.yaml", CHART),
        ("charts/app/templates/_helpers.tpl", HELPERS),
        (
            "charts/app/templates/deployment.yaml",
            "spec:\n  initContainers:\n    - name: {{ include \"chart.waitForDb\" . }}\n",
        ),
        ("charts/other/Chart.yaml", CHART),
        ("charts/other/templates/_helpers.tpl", other),
        (
            "charts/other/templates/job.yaml",
            "spec:\n  command: {{ include \"chart.waitForDb\" . }}\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("charts/app/templates/deployment.yaml"),
        [(
            3,
            "chart.waitForDb in charts/app/templates/_helpers.tpl".to_string()
        )]
    );
    assert_eq!(
        fixture.lands("charts/app/templates/_helpers.tpl"),
        [(
            6,
            "chart.waitForDb in charts/app/templates/_helpers.tpl".to_string()
        )]
    );
    assert_eq!(
        fixture.lands("charts/other/templates/job.yaml"),
        [(
            2,
            "chart.waitForDb in charts/other/templates/_helpers.tpl".to_string()
        )]
    );
}

#[test]
fn a_subchart_includes_the_named_templates_of_the_chart_that_holds_it() {
    let fixture = open(&[
        ("Chart.yaml", CHART),
        ("templates/_helpers.tpl", HELPERS),
        ("charts/sub/Chart.yaml", CHART),
        (
            "charts/sub/templates/job.yaml",
            "spec:\n  command: {{ include \"chart.waitForDb\" . }}\n  other: {{ include \"nowhere\" . }}\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("charts/sub/templates/job.yaml"),
        [
            (2, "chart.waitForDb in templates/_helpers.tpl".to_string()),
            (3, "unresolved(NoBinding)".to_string()),
        ]
    );
}
