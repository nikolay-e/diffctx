mod build_system;
mod cicd;
mod docker;
mod generic;
mod helm;
mod keys;
mod keys_extract;
mod kubernetes;
mod routes;
mod tech;

use super::base::EdgeBuilder;

/// The config builders the Dockerfile and configuration cells did not replace
/// (`CELL_EDGES`).
pub fn get_config_builders() -> Vec<Box<dyn EdgeBuilder>> {
    text_builders()
        .into_iter()
        .filter(|b| !super::cells::replaces(&b.knife_name()))
        .collect()
}

pub(super) fn text_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(generic::ConfigToCodeEdgeBuilder),
        Box::new(cicd::CICDEdgeBuilder),
        Box::new(docker::DockerEdgeBuilder),
        Box::new(build_system::BuildSystemEdgeBuilder),
        Box::new(helm::HelmEdgeBuilder),
        Box::new(kubernetes::KubernetesEdgeBuilder),
        Box::new(keys::KeyTableEdgeBuilder),
    ]
}

pub(crate) use keys::{counterparts_of_change, typed_links};
