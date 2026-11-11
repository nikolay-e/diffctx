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

/// The config builders, each narrowed to what the Dockerfile and configuration
/// cells do not cover when those are drawn (`CELL_EDGES`).
pub fn get_config_builders() -> Vec<Box<dyn EdgeBuilder>> {
    text_builders()
        .into_iter()
        .map(super::cells::beside)
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
