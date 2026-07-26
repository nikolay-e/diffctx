pub mod cochange;

use super::base::EdgeBuilder;

pub fn get_history_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![Box::new(cochange::CochangeEdgeBuilder)]
}
