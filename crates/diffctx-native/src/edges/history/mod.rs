pub(crate) mod cochange;

use super::base::EdgeBuilder;

pub(crate) fn get_history_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![Box::new(cochange::CochangeEdgeBuilder)]
}
