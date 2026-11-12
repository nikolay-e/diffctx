mod lexical;

use super::base::EdgeBuilder;

pub(crate) fn get_similarity_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![Box::new(lexical::LexicalEdgeBuilder)]
}
