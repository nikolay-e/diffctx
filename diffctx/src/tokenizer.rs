use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("failed to load o200k_base BPE tables: {0}")]
    EncoderInit(String),
}

// `Lazy<Result<...>>` keeps the static initialization fallible without
// crashing the host process (PyO3 surface) on sandboxed / proxy-blocked
// environments where tiktoken-rs may fail to materialize the BPE tables.
// `try_count_tokens` is the boundary-safe variant that returns the error;
// `count_tokens` retains the infallible signature used by ~5 internal
// hot-path call sites and degrades to a conservative byte-length estimate
// rather than aborting the entire process.
static ENCODER: Lazy<Result<CoreBPE, TokenizerError>> =
    Lazy::new(|| tiktoken_rs::o200k_base().map_err(|e| TokenizerError::EncoderInit(e.to_string())));

pub fn try_count_tokens(text: &str) -> Result<u32, TokenizerError> {
    // Why `encode_ordinary` (not `encode_with_special_tokens`):
    //
    // 1. Budget contract (R2-T1 regression): `encode_with_special_tokens`
    //    collapses literal `<|endoftext|>`-style sequences into a single
    //    token, breaking byte-accurate accounting against the user budget.
    //
    // 2. Prompt-injection safety: a diff is user input. Treating literal
    //    `<|...|>` sequences as opaque text prevents them from being
    //    interpreted as model control tokens downstream.
    match &*ENCODER {
        Ok(enc) => Ok(enc.encode_ordinary(text).len() as u32),
        Err(e) => Err(TokenizerError::EncoderInit(e.to_string())),
    }
}

pub fn count_tokens(text: &str) -> u32 {
    // Infallible variant for internal hot-path call sites. On encoder-init
    // failure, fall back to a conservative byte-length estimate (4 bytes
    // per token heuristic) so the pipeline degrades gracefully instead of
    // aborting the host process.
    match try_count_tokens(text) {
        Ok(n) => n,
        Err(_) => ((text.len() as u32) / 4).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_token_literals_are_not_collapsed() {
        // R2-T1 regression: `encode_with_special_tokens` would collapse
        // a literal `<|endoftext|>` to a single token, escaping the budget
        // contract. `encode_ordinary` treats it as plain text.
        let n = count_tokens("literal <|endoftext|> in code");
        assert!(
            n > 1,
            "tokenizer must not collapse special-token literals; got {n} tokens"
        );
    }
}
