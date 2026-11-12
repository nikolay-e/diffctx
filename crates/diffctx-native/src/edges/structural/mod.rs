pub(crate) mod containment;
pub(crate) mod sibling;
pub(crate) mod testing;

use super::base::EdgeBuilder;

pub(crate) fn get_structural_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(containment::ContainmentEdgeBuilder),
        Box::new(sibling::SiblingEdgeBuilder),
        Box::new(testing::TestEdgeBuilder),
    ]
}
