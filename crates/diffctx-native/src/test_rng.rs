//! The one deterministic PRNG the tests share. Three byte-identical xorshift
//! copies used to live in `ppr.rs`, `objective.rs` and `boltzmann.rs`.

pub(crate) fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

pub(crate) fn random_subset(n: usize, fraction: f64, rng: &mut u64) -> Vec<usize> {
    (0..n)
        .filter(|_| (xorshift(rng) % 1000) as f64 / 1000.0 < fraction)
        .collect()
}
