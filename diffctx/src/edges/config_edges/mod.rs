mod build_system;
mod cicd;
mod docker;
mod generic;
mod helm;
mod kubernetes;

use super::base::EdgeBuilder;

pub fn get_config_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(generic::ConfigToCodeEdgeBuilder),
        Box::new(cicd::CICDEdgeBuilder),
        Box::new(docker::DockerEdgeBuilder),
        Box::new(build_system::BuildSystemEdgeBuilder),
        Box::new(helm::HelmEdgeBuilder),
        Box::new(kubernetes::KubernetesEdgeBuilder),
    ]
}
