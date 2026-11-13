//! The Kubernetes reference table: which field of which kind names an object of
//! which kind, and which kinds stand in a namespace.
//!
//! The table is data, and a field is a reference because a row says so
//! (Revision 11 of the migration plan, item 13). The sources are the API
//! reference of each kind (`PodSpec`, `Volume`, `EnvVarSource`, `IngressBackend`,
//! `RoleBinding`, `CrossVersionObjectReference`) and the Flux and Argo CD
//! resource definitions; a row is added with a fixture that reads it.

use std::sync::LazyLock;

use super::reference::Row;
use super::selectors::{LabelSet, SelectorRow};

/// Kinds that hold a pod template, with the path of the pod spec in each.
static WORKLOADS: &[(&str, &str)] = &[
    ("Pod", "spec"),
    ("Deployment", "spec.template.spec"),
    ("StatefulSet", "spec.template.spec"),
    ("DaemonSet", "spec.template.spec"),
    ("ReplicaSet", "spec.template.spec"),
    ("ReplicationController", "spec.template.spec"),
    ("Job", "spec.template.spec"),
    ("CronJob", "spec.jobTemplate.spec.template.spec"),
];

/// Fields of a pod spec that name an object: (path in the pod spec, kind of the
/// object, the field that names a key inside it).
static POD_FIELDS: &[(&str, &str, Option<&str>)] = &[
    ("volumes[].configMap.name", "ConfigMap", None),
    ("volumes[].secret.secretName", "Secret", None),
    (
        "volumes[].persistentVolumeClaim.claimName",
        "PersistentVolumeClaim",
        None,
    ),
    (
        "volumes[].projected.sources[].configMap.name",
        "ConfigMap",
        None,
    ),
    ("volumes[].projected.sources[].secret.name", "Secret", None),
    ("serviceAccountName", "ServiceAccount", None),
    ("imagePullSecrets[].name", "Secret", None),
];

/// The lists of containers of a pod spec.
static CONTAINER_LISTS: &[&str] = &["containers", "initContainers"];

/// Fields of a container that name an object.
static CONTAINER_FIELDS: &[(&str, &str, Option<&str>)] = &[
    ("envFrom[].configMapRef.name", "ConfigMap", None),
    ("envFrom[].secretRef.name", "Secret", None),
    (
        "env[].valueFrom.configMapKeyRef.name",
        "ConfigMap",
        Some("key"),
    ),
    ("env[].valueFrom.secretKeyRef.name", "Secret", Some("key")),
];

static TABLE: LazyLock<Vec<Row>> = LazyLock::new(build);

/// Every typed row, in a stable order.
pub fn references() -> &'static [Row] {
    &TABLE
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn build() -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, (_, root)) in WORKLOADS.iter().enumerate() {
        let kinds: &'static [&'static str] = std::slice::from_ref(&WORKLOADS[index].0);
        let mut add = |path: String, target: &'static str, key: Option<&'static str>| {
            let row = Row::new(kinds, leak(path), target);
            rows.push(match key {
                Some(field) => row.key(field),
                None => row,
            });
        };
        for (field, target, key) in POD_FIELDS {
            add(format!("{root}.{field}"), *target, *key);
        }
        for list in CONTAINER_LISTS {
            for (field, target, key) in CONTAINER_FIELDS {
                add(format!("{root}.{list}[].{field}"), *target, *key);
            }
        }
    }
    rows.extend([
        Row::new(&["StatefulSet"], "spec.serviceName", "Service"),
        Row::new(
            &["Ingress"],
            "spec.rules[].http.paths[].backend.service.name",
            "Service",
        ),
        Row::new(&["Ingress"], "spec.defaultBackend.service.name", "Service"),
        Row::new(&["Ingress"], "spec.tls[].secretName", "Secret"),
        Row::new(&["Ingress"], "spec.ingressClassName", "IngressClass"),
        Row::new(&["RoleBinding", "ClusterRoleBinding"], "roleRef.name", "").kind_from("kind"),
        Row::new(
            &["RoleBinding", "ClusterRoleBinding"],
            "subjects[].name",
            "ServiceAccount",
        )
        .namespace("namespace")
        .when("kind", "ServiceAccount"),
        Row::new(&["HorizontalPodAutoscaler"], "spec.scaleTargetRef.name", "").kind_from("kind"),
        // Argo Workflows.
        Row::new(
            &["Workflow", "WorkflowTemplate", "ClusterWorkflowTemplate"],
            "spec.workflowTemplateRef.name",
            "WorkflowTemplate",
        ),
        Row::new(
            &["CronWorkflow"],
            "spec.workflowSpec.workflowTemplateRef.name",
            "WorkflowTemplate",
        ),
        Row::new(
            &["Workflow", "WorkflowTemplate", "ClusterWorkflowTemplate"],
            "spec.templates[].dag.tasks[].templateRef.name",
            "WorkflowTemplate",
        ),
        Row::new(
            &["Workflow", "WorkflowTemplate", "ClusterWorkflowTemplate"],
            "spec.templates[].steps[][].templateRef.name",
            "WorkflowTemplate",
        ),
        Row::new(
            &["CronWorkflow"],
            "spec.workflowSpec.templates[].steps[][].templateRef.name",
            "WorkflowTemplate",
        ),
        Row::new(
            &["CronWorkflow"],
            "spec.workflowSpec.templates[].dag.tasks[].templateRef.name",
            "WorkflowTemplate",
        ),
        // Flux.
        Row::new(&["Kustomization"], "spec.sourceRef.name", "")
            .kind_from("kind")
            .namespace("namespace"),
        Row::new(&["Kustomization"], "spec.dependsOn[].name", "Kustomization")
            .namespace("namespace"),
        Row::new(
            &["Kustomization", "HelmRelease"],
            "spec.kubeConfig.secretRef.name",
            "Secret",
        ),
        Row::new(
            &["Kustomization"],
            "spec.decryption.secretRef.name",
            "Secret",
        ),
        Row::new(
            &["Kustomization"],
            "spec.postBuild.substituteFrom[].name",
            "",
        )
        .kind_from("kind"),
        Row::new(
            &["Kustomization", "HelmRelease"],
            "spec.serviceAccountName",
            "ServiceAccount",
        ),
        Row::new(&["HelmRelease"], "spec.chart.spec.sourceRef.name", "")
            .kind_from("kind")
            .namespace("namespace"),
        Row::new(&["HelmRelease"], "spec.valuesFrom[].name", "").kind_from("kind"),
        Row::new(&["HelmRelease"], "spec.dependsOn[].name", "HelmRelease").namespace("namespace"),
    ]);
    rows
}

/// Kinds of the core API and of the groups Kubernetes ships that stand in no
/// namespace.
static CLUSTER_SCOPED: &[&str] = &[
    "APIService",
    "CertificateSigningRequest",
    "ClusterRole",
    "ClusterRoleBinding",
    "ClusterWorkflowTemplate",
    "CSIDriver",
    "CSINode",
    "CustomResourceDefinition",
    "IngressClass",
    "MutatingWebhookConfiguration",
    "Namespace",
    "Node",
    "PersistentVolume",
    "PriorityClass",
    "RuntimeClass",
    "StorageClass",
    "ValidatingWebhookConfiguration",
    "VolumeAttachment",
];

/// Kinds of the core API and of the groups Kubernetes ships that stand in a
/// namespace.
static NAMESPACED: &[&str] = &[
    "ConfigMap",
    "CronJob",
    "DaemonSet",
    "Deployment",
    "EndpointSlice",
    "Endpoints",
    "Event",
    "HorizontalPodAutoscaler",
    "Ingress",
    "Job",
    "Lease",
    "LimitRange",
    "NetworkPolicy",
    "PersistentVolumeClaim",
    "Pod",
    "PodDisruptionBudget",
    "ReplicaSet",
    "ReplicationController",
    "ResourceQuota",
    "Role",
    "RoleBinding",
    "Secret",
    "Service",
    "ServiceAccount",
    "StatefulSet",
];

/// Kinds whose pods a selector can select, the workloads.
const SELECTABLE_WORKLOADS: &[&str] = &[
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "ReplicaSet",
    "Job",
];

/// The selectors: the map at the path selects objects whose labels hold every
/// pair. A Service selects pods by the pairs of `spec.selector`; the other
/// selectors by their `matchLabels` and `matchExpressions`. A workload selects the pods its own
/// template makes, and any other pod that carries the labels.
static SELECTORS: &[SelectorRow] = &[
    SelectorRow {
        kinds: &["Service"],
        path: "spec.selector",
        label_selector: false,
        matches: LabelSet::Pod,
        target_kinds: &[],
    },
    SelectorRow {
        kinds: &["PodDisruptionBudget"],
        path: "spec.selector",
        label_selector: true,
        matches: LabelSet::Pod,
        target_kinds: &[],
    },
    SelectorRow {
        kinds: &["NetworkPolicy"],
        path: "spec.podSelector",
        label_selector: true,
        matches: LabelSet::Pod,
        target_kinds: &[],
    },
    SelectorRow {
        kinds: SELECTABLE_WORKLOADS,
        path: "spec.selector",
        label_selector: true,
        matches: LabelSet::Pod,
        target_kinds: &[],
    },
    SelectorRow {
        kinds: &["ServiceMonitor"],
        path: "spec.selector",
        label_selector: true,
        matches: LabelSet::Own,
        target_kinds: &["Service"],
    },
    SelectorRow {
        kinds: &["PodMonitor"],
        path: "spec.selector",
        label_selector: true,
        matches: LabelSet::Pod,
        target_kinds: &[],
    },
];

pub fn selector_rows() -> &'static [SelectorRow] {
    SELECTORS
}

/// Where a workload's pod template keeps its labels. A `Pod` has none: its own
/// labels are its pod's.
pub fn pod_labels_path(kind: &str) -> Option<&'static str> {
    match kind {
        "Deployment"
        | "StatefulSet"
        | "DaemonSet"
        | "ReplicaSet"
        | "ReplicationController"
        | "Job" => Some("spec.template.metadata.labels"),
        "CronJob" => Some("spec.jobTemplate.spec.template.metadata.labels"),
        _ => None,
    }
}

/// Whether objects of `kind` stand in a namespace: `None` for a kind the
/// table does not know (a custom resource, whose scope its definition states
/// and no row here reads).
pub fn is_namespaced(kind: &str) -> Option<bool> {
    if CLUSTER_SCOPED.contains(&kind) {
        Some(false)
    } else if NAMESPACED.contains(&kind) {
        Some(true)
    } else {
        None
    }
}
