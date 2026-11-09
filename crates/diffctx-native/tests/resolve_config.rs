//! The configuration resolver on real git repositories: a field that names an
//! object finds the object by kind, name and namespace, and what it cannot tell
//! apart it says so. The expectations are read off the fixtures; nothing here
//! runs diffctx to learn an answer.

mod snapshot_fixture;

use _diffctx::facts::config::identity_of;
use _diffctx::facts::{FactsCache, LocalKey, Namespace};
use _diffctx::resolve::{Binding, CandidateReason, ConfigResolver, Relation, Resolution};
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
    /// `Kind/name@namespace in file`, for a key of the snapshot; a member of an
    /// object (its labels) is `Kind/name@namespace.member`.
    fn where_is(&self, key: &_diffctx::facts::DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition");
        let (object, member) = match key.local.container.first() {
            Some(parent) => {
                let mut local = LocalKey::new(Vec::new(), parent.clone(), Namespace::Value);
                local.disambig.clone_from(&key.local.disambig);
                (local, format!(".{}", key.local.name))
            }
            None => (key.local.clone(), String::new()),
        };
        let id = identity_of(&object).expect("an object key");
        format!(
            "{}/{}@{}{} in {}",
            id.kind,
            id.name,
            id.namespace.as_deref().unwrap_or("-"),
            member,
            found.file
        )
    }

    fn reads(&self, file: &str) -> Vec<&Binding> {
        self.head
            .bindings()
            .iter()
            .filter(|b| b.file.as_str() == file && b.relation == Relation::Reads)
            .collect()
    }

    /// What each read of a file lands on, by the line of the read.
    fn lands(&self, file: &str) -> Vec<(u32, String)> {
        self.reads(file)
            .into_iter()
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

const STATEFULSET: &str = "apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: db
spec:
  serviceName: db-svc
  template:
    spec:
      containers:
        - name: db
          envFrom:
            - configMapRef:
                name: app-settings
";

#[test]
fn a_field_of_the_table_lands_on_the_object_of_its_kind_and_name() {
    let fixture = open(&[
        ("k8s/statefulset.yaml", STATEFULSET),
        (
            "k8s/service.yaml",
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: db-svc\n",
        ),
        (
            "k8s/other.yaml",
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: other-svc\n",
        ),
        (
            "k8s/configmap.yaml",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: app-settings\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("k8s/statefulset.yaml"),
        [
            (6, "Service/db-svc@- in k8s/service.yaml".to_string()),
            (
                13,
                "ConfigMap/app-settings@- in k8s/configmap.yaml".to_string()
            ),
        ]
    );
}

#[test]
fn the_kind_the_field_names_decides_between_objects_of_one_name() {
    let fixture = open(&[
        (
            "app.yaml",
            "apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: db
spec:
  serviceName: db
",
        ),
        (
            "svc.yaml",
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: db\n",
        ),
        (
            "cm.yaml",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: db\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("app.yaml"),
        [(6, "Service/db@- in svc.yaml".to_string())]
    );
}

#[test]
fn a_namespace_the_referrer_has_excludes_the_objects_of_another() {
    let workload = |namespace: &str| {
        format!(
            "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
{namespace}spec:
  template:
    spec:
      serviceAccountName: runner
"
        )
    };
    let account = |namespace: &str| {
        format!("apiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: runner\n{namespace}")
    };
    let fixture = open(&[
        ("web.yaml", workload("  namespace: a\n").as_str()),
        ("a/sa.yaml", account("  namespace: a\n").as_str()),
        ("b/sa.yaml", account("  namespace: b\n").as_str()),
    ]);
    assert_eq!(
        fixture.lands("web.yaml"),
        [(9, "ServiceAccount/runner@a in a/sa.yaml".to_string())]
    );
}

#[test]
fn a_referrer_without_a_namespace_cannot_tell_two_namespaces_apart() {
    let fixture = open(&[
        (
            "web.yaml",
            "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  template:
    spec:
      serviceAccountName: runner
",
        ),
        (
            "a/sa.yaml",
            "apiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: runner\n  namespace: a\n",
        ),
        (
            "b/sa.yaml",
            "apiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: runner\n  namespace: b\n",
        ),
    ]);
    let landing = fixture.lands("web.yaml");
    assert_eq!(landing.len(), 1);
    assert_eq!(
        landing[0].1,
        "candidate(AnyNamespace) ServiceAccount/runner@a in a/sa.yaml | ServiceAccount/runner@b in b/sa.yaml"
    );
    assert!(matches!(
        fixture.reads("web.yaml")[0].resolution,
        Resolution::Candidate {
            reason: CandidateReason::AnyNamespace,
            ..
        }
    ));
}

#[test]
fn an_object_without_a_namespace_answers_where_none_states_it() {
    let fixture = open(&[
        (
            "web.yaml",
            "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: shop
spec:
  template:
    spec:
      serviceAccountName: runner
",
        ),
        (
            "sa.yaml",
            "apiVersion: v1\nkind: ServiceAccount\nmetadata:\n  name: runner\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("web.yaml"),
        [(9, "ServiceAccount/runner@- in sa.yaml".to_string())]
    );
}

#[test]
fn a_cluster_scoped_kind_ignores_the_namespace_and_the_field_names_the_kind() {
    let fixture = open(&[
        (
            "rb.yaml",
            "apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: rb
  namespace: shop
roleRef:
  kind: ClusterRole
  name: viewer
subjects:
  - kind: ServiceAccount
    name: runner
  - kind: User
    name: alice
",
        ),
        (
            "clusterrole.yaml",
            "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: viewer\n",
        ),
        (
            "role.yaml",
            "apiVersion: rbac.authorization.k8s.io/v1\nkind: Role\nmetadata:\n  name: viewer\n  namespace: shop\n",
        ),
    ]);
    // The user `alice` is no object; the service account `runner` stands
    // nowhere in the repository.
    assert_eq!(
        fixture.lands("rb.yaml"),
        [
            (8, "ClusterRole/viewer@- in clusterrole.yaml".to_string()),
            (11, "unresolved(External)".to_string()),
        ]
    );
}

#[test]
fn a_suffix_field_is_a_candidate_for_an_object_of_that_name_and_never_for_itself() {
    let fixture = open(&[
        (
            "pvc.yaml",
            "apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: data
spec:
  storageClassName: fast
  volumeName: data
",
        ),
        (
            "sc.yaml",
            "apiVersion: storage.k8s.io/v1\nkind: StorageClass\nmetadata:\n  name: fast\n",
        ),
    ]);
    // `volumeName: data` names the claim's own name: no candidate for itself.
    assert_eq!(
        fixture.lands("pvc.yaml"),
        [(
            6,
            "candidate(NameSuffix) StorageClass/fast@- in sc.yaml".to_string()
        )]
    );
}

#[test]
fn a_place_a_kustomization_names_lands_on_the_file_its_tool_reads_there() {
    let fixture = open(&[
        (
            "overlay/kustomization.yaml",
            "resources:
  - ../base
  - deploy.yaml
  - missing.yaml
  - https://example.com/x.yaml
",
        ),
        ("base/kustomization.yaml", "resources: []\n"),
        (
            "overlay/deploy.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n",
        ),
    ]);
    assert_eq!(fixture.head.external_sources(), 1);
    assert_eq!(fixture.head.templated_files(), 0);
    let places: Vec<(u32, String)> = fixture
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "overlay/kustomization.yaml")
        .map(|b| (b.site.lines.0, fixture.describe(b)))
        .collect();
    assert_eq!(
        places,
        [
            (2, "module base/kustomization.yaml".to_string()),
            (3, "module overlay/deploy.yaml".to_string()),
            (4, "unresolved(MissingModule)".to_string()),
        ]
    );
}

fn workload(name: &str, namespace: &str, labels: &str) -> String {
    format!(
        "apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  namespace: {namespace}
spec:
  template:
    metadata:
      labels:
        {labels}
    spec:
      containers: []
"
    )
}

#[test]
fn a_service_selector_is_a_candidate_for_the_pod_labels_it_holds_in_its_namespace() {
    let fixture = open(&[
        ("web.yaml", workload("web", "shop", "app: web").as_str()),
        ("api.yaml", workload("api", "shop", "app: api").as_str()),
        (
            "other.yaml",
            workload("web", "staging", "app: web").as_str(),
        ),
        (
            "svc.yaml",
            "apiVersion: v1
kind: Service
metadata:
  name: web
  namespace: shop
spec:
  selector:
    app: web
",
        ),
    ]);
    assert_eq!(
        fixture.lands("svc.yaml"),
        [(
            8,
            "candidate(LabelSelector) Deployment/web@shop.pod-labels in web.yaml".to_string()
        )]
    );
    assert!(matches!(
        fixture.reads("svc.yaml")[0].resolution,
        Resolution::Candidate {
            reason: CandidateReason::LabelSelector,
            ..
        }
    ));
}

#[test]
fn a_selector_needs_every_pair_and_a_workload_does_not_select_itself() {
    let fixture = open(&[
        (
            "web.yaml",
            "apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers: []
",
        ),
        (
            "pdb.yaml",
            "apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: web
spec:
  selector:
    matchLabels:
      app: web
      tier: ui
",
        ),
        (
            "np.yaml",
            "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: web
spec:
  podSelector:
    matchLabels:
      app: web
",
        ),
    ]);
    // The workload's own selector holds its own template's labels and no other.
    assert_eq!(
        fixture.lands("web.yaml"),
        [(8, "unresolved(NoMatch)".to_string())]
    );
    // `tier: ui` is held by no pod.
    assert_eq!(
        fixture.lands("pdb.yaml"),
        [(8, "unresolved(NoMatch)".to_string())]
    );
    assert_eq!(
        fixture.lands("np.yaml"),
        [(
            8,
            "candidate(LabelSelector) Deployment/web@-.pod-labels in web.yaml".to_string()
        )]
    );
}
