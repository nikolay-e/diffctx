pub mod containment;
pub mod sibling;
pub mod testing;

use super::base::EdgeBuilder;

pub fn get_structural_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(containment::ContainmentEdgeBuilder),
        Box::new(sibling::SiblingEdgeBuilder),
        Box::new(testing::TestEdgeBuilder),
    ]
}
