use once_cell::sync::Lazy;

pub struct Bm25Config {
    pub k1: f64,
    pub b: f64,
    pub doc_len_factor: f64,
    pub idf_smoothing: f64,
    pub min_query_token_length: usize,
}

impl Default for Bm25Config {
    fn default() -> Self {
        Self {
            k1: 2.5,
            b: 0.75,
            doc_len_factor: 1.5,
            idf_smoothing: 0.5,
            min_query_token_length: 3,
        }
    }
}

pub static BM25: Lazy<Bm25Config> = Lazy::new(Bm25Config::default);
